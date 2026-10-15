use amdgpu_sysfs::hw_mon::Temperature;
use lact_schema::{PowerLimitThrottleStatus, config::PowerLimitThrottle};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Measurements used for a single power limit throttling check.
#[derive(Debug, Clone, Copy)]
pub struct ThrottleObservation<'a> {
    pub now: Instant,
    /// Power limit currently set on the device, in watts.
    pub current_cap: f64,
    pub temps: &'a HashMap<String, Temperature>,
    /// Power usage of the device in watts, when it can be read.
    pub power: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThrottleAction {
    /// The power limit does not need to be changed.
    Hold,
    /// The power limit should be set to this value in watts.
    SetCap { cap: f64 },
}

/// Lowers the power limit when a configured temperature sensor is too hot,
/// and raises it back towards the configured limit when the sensors have cooled down.
///
/// The current power limit is always taken from the device instead of being tracked
/// internally, so that an out-of-band change of the limit is picked up automatically.
#[derive(Debug, Clone)]
pub struct PowerThrottle {
    settings: PowerLimitThrottle,
    base_cap: f64,
    effective_cap: f64,
    lock_until: Option<Instant>,
    breached: Vec<String>,
}

impl PowerThrottle {
    pub fn new(settings: PowerLimitThrottle, base_cap: f64) -> Self {
        Self {
            // The base limit is the highest one that can ever be set.
            settings: PowerLimitThrottle {
                min_power: settings.min_power.min(base_cap),
                ..settings
            },
            base_cap,
            effective_cap: base_cap,
            lock_until: None,
            breached: Vec::new(),
        }
    }

    #[must_use]
    pub fn status(&self) -> PowerLimitThrottleStatus {
        PowerLimitThrottleStatus {
            base_cap: self.base_cap,
            effective_cap: self.effective_cap,
            load_locked: self.lock_until.is_some(),
            breached: self.breached.clone(),
        }
    }

    pub fn evaluate(&mut self, observation: &ThrottleObservation) -> ThrottleAction {
        self.effective_cap = observation.current_cap;
        self.update_load_lock(observation);
        self.breached = self.breached_sensors(observation);

        if !self.breached.is_empty() {
            return self.lower_cap(observation);
        }

        if self.all_cooled(observation)
            && observation.current_cap < self.base_cap
            && self.lock_until.is_none()
        {
            let cap = (observation.current_cap + self.settings.step).min(self.base_cap);
            self.effective_cap = cap;
            return ThrottleAction::SetCap { cap };
        }

        ThrottleAction::Hold
    }

    fn lower_cap(&mut self, observation: &ThrottleObservation) -> ThrottleAction {
        if observation.current_cap <= self.settings.min_power {
            return ThrottleAction::Hold;
        }
        let cap = (observation.current_cap - self.settings.step).max(self.settings.min_power);
        self.effective_cap = cap;
        ThrottleAction::SetCap { cap }
    }

    /// Keeps the "raise the limit" lock active while the device is under a heavy load.
    fn update_load_lock(&mut self, observation: &ThrottleObservation) {
        let Some(power) = observation.power else {
            return;
        };
        let Some(load_lock) = &self.settings.load_lock else {
            return;
        };

        let load_threshold = observation.current_cap * f64::from(load_lock.power_pct) / 100.0;
        if power >= load_threshold {
            let until = observation.now + Duration::from_secs(load_lock.duration_secs);
            self.lock_until = Some(self.lock_until.unwrap_or(observation.now).max(until));
        }
        if self
            .lock_until
            .is_some_and(|until| observation.now >= until)
        {
            self.lock_until = None;
        }
    }

    fn breached_sensors(&self, observation: &ThrottleObservation) -> Vec<String> {
        let mut breached: Vec<String> = self
            .settings
            .thresholds
            .iter()
            .filter_map(|(name, max)| {
                let over_threshold = observation
                    .temps
                    .get(name)
                    .and_then(|temp| temp.current)
                    .is_some_and(|current| f64::from(current) >= f64::from(*max));

                over_threshold.then(|| name.clone())
            })
            .collect();
        breached.sort();
        breached
    }

    /// True when every configured sensor reads below its threshold minus the hysteresis.
    /// A sensor which cannot be read at all prevents the limit from being raised.
    fn all_cooled(&self, observation: &ThrottleObservation) -> bool {
        self.settings.thresholds.iter().all(|(name, max)| {
            observation
                .temps
                .get(name)
                .and_then(|temp| temp.current)
                .is_some_and(|current| {
                    f64::from(current) <= f64::from(*max) - f64::from(self.settings.hysteresis)
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lact_schema::config::PowerThrottleLoadLock;

    const BASE_CAP: f64 = 333.0;

    fn settings() -> PowerLimitThrottle {
        PowerLimitThrottle {
            min_power: 261.0,
            thresholds: [("junction".to_owned(), 92), ("mem".to_owned(), 95)].into(),
            step: 10.0,
            interval_secs: 1,
            hysteresis: 5,
            load_lock: None,
        }
    }

    fn with_load_lock() -> PowerLimitThrottle {
        PowerLimitThrottle {
            load_lock: Some(PowerThrottleLoadLock {
                power_pct: 90,
                duration_secs: 300,
            }),
            ..settings()
        }
    }

    fn temps(entries: &[(&str, f32)]) -> HashMap<String, Temperature> {
        entries
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_owned(),
                    Temperature {
                        current: Some(*value),
                        crit: None,
                        crit_hyst: None,
                    },
                )
            })
            .collect()
    }

    fn cool_temps() -> HashMap<String, Temperature> {
        temps(&[("junction", 70.0), ("mem", 70.0)])
    }

    #[test]
    fn lowers_limit_when_sensor_is_hot() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = temps(&[("junction", 92.0), ("mem", 70.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: BASE_CAP,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 323.0 });
        assert_eq!(throttle.status().breached, ["junction".to_owned()]);
    }

    #[test]
    fn lowers_limit_when_vram_is_hot() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = temps(&[("junction", 70.0), ("mem", 96.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: BASE_CAP,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 323.0 });
    }

    #[test]
    fn holds_limit_between_threshold_and_hysteresis() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = temps(&[("junction", 89.0), ("mem", 91.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: 300.0,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::Hold);
    }

    #[test]
    fn raises_limit_when_sensors_are_cooled() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = cool_temps();

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: 300.0,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 310.0 });
    }

    #[test]
    fn never_raises_above_base_limit() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = cool_temps();

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: 330.0,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: BASE_CAP });

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: BASE_CAP,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::Hold);
    }

    #[test]
    fn never_lowers_below_minimum() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = temps(&[("junction", 100.0), ("mem", 100.0)]);

        let observation = ThrottleObservation {
            now: Instant::now(),
            current_cap: 265.0,
            temps: &device_temps,
            power: None,
        };
        assert_eq!(
            throttle.evaluate(&observation),
            ThrottleAction::SetCap { cap: 261.0 }
        );

        let observation = ThrottleObservation {
            current_cap: 261.0,
            ..observation
        };
        assert_eq!(throttle.evaluate(&observation), ThrottleAction::Hold);
    }

    #[test]
    fn minimum_above_base_limit_does_not_lower() {
        let settings = PowerLimitThrottle {
            min_power: 400.0,
            ..settings()
        };
        let mut throttle = PowerThrottle::new(settings, BASE_CAP);
        let device_temps = temps(&[("junction", 100.0), ("mem", 100.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: BASE_CAP,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::Hold);
    }

    #[test]
    fn load_lock_blocks_raising_until_expired() {
        let mut throttle = PowerThrottle::new(with_load_lock(), BASE_CAP);
        let device_temps = cool_temps();
        let now = Instant::now();

        // A heavy load seen at this sample blocks raising the limit.
        let action = throttle.evaluate(&ThrottleObservation {
            now,
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(290.0),
        });
        assert_eq!(action, ThrottleAction::Hold);
        assert!(throttle.status().load_locked);

        // The load is gone, but the lock is still active.
        let action = throttle.evaluate(&ThrottleObservation {
            now: now + Duration::from_secs(60),
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(10.0),
        });
        assert_eq!(action, ThrottleAction::Hold);

        // The lock expired, the limit can be raised again.
        let action = throttle.evaluate(&ThrottleObservation {
            now: now + Duration::from_secs(301),
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(10.0),
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 310.0 });
        assert!(!throttle.status().load_locked);
    }

    #[test]
    fn load_lock_is_extended_by_new_load() {
        let mut throttle = PowerThrottle::new(with_load_lock(), BASE_CAP);
        let device_temps = cool_temps();
        let now = Instant::now();

        throttle.evaluate(&ThrottleObservation {
            now,
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(290.0),
        });
        throttle.evaluate(&ThrottleObservation {
            now: now + Duration::from_secs(299),
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(290.0),
        });

        let action = throttle.evaluate(&ThrottleObservation {
            now: now + Duration::from_secs(301),
            current_cap: 300.0,
            temps: &device_temps,
            power: Some(10.0),
        });
        assert_eq!(action, ThrottleAction::Hold);
    }

    #[test]
    fn load_does_not_block_lowering() {
        let mut throttle = PowerThrottle::new(with_load_lock(), BASE_CAP);
        let device_temps = temps(&[("junction", 95.0), ("mem", 70.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: BASE_CAP,
            temps: &device_temps,
            power: Some(330.0),
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 323.0 });
    }

    #[test]
    fn missing_power_sensor_disables_load_lock() {
        let mut throttle = PowerThrottle::new(with_load_lock(), BASE_CAP);
        let device_temps = cool_temps();

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: 300.0,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::SetCap { cap: 310.0 });
    }

    #[test]
    fn unreadable_sensor_prevents_raising() {
        let mut throttle = PowerThrottle::new(settings(), BASE_CAP);
        let device_temps = temps(&[("junction", 40.0)]);

        let action = throttle.evaluate(&ThrottleObservation {
            now: Instant::now(),
            current_cap: 300.0,
            temps: &device_temps,
            power: None,
        });
        assert_eq!(action, ThrottleAction::Hold);
    }
}

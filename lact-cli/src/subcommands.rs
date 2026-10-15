use crate::CliContext;
use anyhow::{Context, Result, bail};
use lact_schema::{
    FanControlMode,
    args::cli::{
        PowerLimitCmd, PowerThrottleCmd, PowerThrottleSetArgs, ProfileArgs, ProfileAutoSwitchArgs,
        SetProfileArgs,
    },
    config::{
        PowerLimitThrottle, PowerThrottleLoadLock, default_load_lock_duration_secs,
        default_load_lock_power_pct, default_power_throttle_hysteresis,
        default_power_throttle_interval_secs, default_power_throttle_step,
    },
};
use std::fmt::Write;

const PROFILE_DEFAULT: &str = "Default";

pub async fn list_gpus(ctx: CliContext<'_>) -> Result<()> {
    let entries = ctx.client.list_devices().await?;
    for (i, entry) in entries.into_iter().enumerate() {
        if entry.name.is_some() {
            println!("{i}: {} ({entry}) [{}]", entry.id, entry.device_type);
        } else {
            println!("{i}: {} [{}]", entry.id, entry.device_type);
        }
    }
    Ok(())
}

pub async fn info(ctx: CliContext<'_>) -> Result<()> {
    let id = ctx.current_gpu_id().await?;

    let gpu_line = format!("GPU {id}:");
    println!("{gpu_line}");
    println!("{}", "=".repeat(gpu_line.len()));

    let info = ctx.client.get_device_info(&id, Some(false)).await?;
    let stats = ctx.client.get_device_stats(&id).await?;

    let elements = info.info_elements(Some(&stats));
    for (name, value) in elements {
        if let Some(value) = value {
            println!("{name}: {value}");
        }
    }

    Ok(())
}

pub async fn stats(ctx: CliContext<'_>) -> Result<()> {
    let id = ctx.current_gpu_id().await?;

    let gpu_line = format!("GPU {id}:");
    println!("{gpu_line}");
    println!("{}", "=".repeat(gpu_line.len()));

    // let info = ctx.client.get_device_info(&id).await?;
    let stats = ctx.client.get_device_stats(&id).await?;

    if let Some(gpu_clock) = stats.clockspeed.gpu_clockspeed {
        println!("GPU Clockspeed: {gpu_clock} MHz");
    }

    if let Some(vram_clock) = stats.clockspeed.vram_clockspeed {
        println!("VRAM Clockspeed: {vram_clock} MHz");
    }

    if let Some(gpu_voltage) = stats.voltage.gpu {
        println!("GPU Voltage: {gpu_voltage} mV");
    }

    if let (Some(power_usage), Some(power_cap)) = (stats.power.current, stats.power.cap_current) {
        println!("Power Usage: {power_usage:.1}/{power_cap} W");
    }

    for (label, value) in &stats.power.sensors {
        println!("Power Sensor {label}: {value:.1} W");
    }

    if let Some(throttle) = &stats.power_limit_throttle {
        print!(
            "Power Limit Throttling: {} W -> {} W",
            throttle.base_cap, throttle.effective_cap
        );
        if !throttle.breached.is_empty() {
            print!(" (over threshold: {})", throttle.breached.join(", "));
        }
        if throttle.load_locked {
            print!(" (raising blocked due to load)");
        }
        println!();
    }

    if !stats.temps.is_empty() {
        print!("Temperatures: ");
        for (i, (name, value)) in stats.temps.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            if let Some(value) = value.value.current {
                print!("{name}: {value}°C");
            }
        }
        println!();
    }

    if let (Some(vram_current), Some(vram_total)) = (stats.vram.used, stats.vram.total) {
        println!(
            "VRAM Usage: {}/{} MiB",
            vram_current / 1024 / 1024,
            vram_total / 1024 / 1024
        );
    }

    if let Some(throttle_info) = stats.throttle_info {
        let type_text: Vec<String> = throttle_info
            .iter()
            .map(|(throttle_type, details)| {
                let mut out = throttle_type.to_string();
                if !details.is_empty() {
                    let _ = write!(out, "({})", details.join(", "));
                }
                out
            })
            .collect();

        println!(
            "Throttling: {}",
            if type_text.is_empty() {
                "No".to_owned()
            } else {
                type_text.join(", ")
            }
        );
    }

    if let Some(pwm) = stats.fan.pwm_current {
        print!("Fan Speed: {:.0}%", pwm as f64 / 255.0 * 100.0);

        if let Some(rpm) = stats.fan.speed_current {
            print!(" ({rpm} RPM)");
        }

        println!();
    }

    println!(
        "Fan Control Mode: {}",
        if stats.fan.control_enabled {
            match stats.fan.control_mode {
                Some(FanControlMode::Curve) => "Curve",
                Some(FanControlMode::Static) => "Static",
                None => panic!("Invalid fan control config"),
            }
        } else {
            "Automatic"
        }
    );

    Ok(())
}

pub async fn snapshot(ctx: CliContext<'_>) -> Result<()> {
    let path = ctx.client.generate_debug_snapshot().await?;
    println!("Generated debug snapshot in {path}");
    Ok(())
}

pub async fn power_limit(ctx: CliContext<'_>, cmd: Option<&PowerLimitCmd>) -> Result<()> {
    let id = ctx.current_gpu_id().await?;
    match cmd {
        Some(PowerLimitCmd::Get) | None => {
            let stats = ctx.client.get_device_stats(&id).await?;
            let cap = stats
                .power
                .cap_current
                .context("No cap reported by the GPU")?;

            print!("Current power limit: {cap}W");

            if let (Some(min), Some(max)) = (stats.power.cap_min, stats.power.cap_max) {
                print!(" (Configurable Range: {min}W to {max}W)");
            }

            if let Some(throttle) = stats.power_limit_throttle {
                print!(" (throttled down from {}W)", throttle.base_cap);
            }
            println!();
        }
        Some(PowerLimitCmd::Set { limit }) => {
            ctx.edit_gpu_config(&id, |config| {
                config.power_cap = Some((*limit).into());
            })
            .await?;
            println!("Updated power limit to {limit}W");
        }
        Some(PowerLimitCmd::Throttle { cmd }) => match cmd {
            None | Some(PowerThrottleCmd::Get) => show_power_throttle(&id, &ctx).await?,
            Some(PowerThrottleCmd::Set(args)) => set_power_throttle(&id, args, &ctx).await?,
            Some(PowerThrottleCmd::Off) => {
                ctx.edit_gpu_config(&id, |config| {
                    config.power_limit_throttle = None;
                })
                .await?;
                println!("Disabled power limit throttling");
            }
        },
    }
    Ok(())
}

async fn show_power_throttle(id: &str, ctx: &CliContext<'_>) -> Result<()> {
    let config = ctx.client.get_gpu_config(id).await?;
    let Some(throttle) = config.and_then(|config| config.power_limit_throttle) else {
        println!("Power limit throttling: disabled");
        return Ok(());
    };

    println!("Power limit throttling: enabled");
    print_throttle_settings(&throttle);

    match ctx.client.get_device_stats(id).await?.power_limit_throttle {
        Some(status) => {
            print!(
                "Current state: power limit {}W (not above {}W)",
                status.effective_cap, status.base_cap
            );
            if !status.breached.is_empty() {
                print!(", over threshold: {}", status.breached.join(", "));
            }
            if status.load_locked {
                print!(", raising blocked due to load");
            }
            println!();
        }
        None => println!("Current state: power limit is not lowered"),
    }

    Ok(())
}

async fn set_power_throttle(
    id: &str,
    args: &PowerThrottleSetArgs,
    ctx: &CliContext<'_>,
) -> Result<()> {
    let stats = ctx.client.get_device_stats(id).await?;
    let lowest_cap = stats
        .power
        .cap_min
        .context("Could not get the lowest power limit of the GPU")?;

    let existing = ctx.client.get_gpu_config(id).await?;
    let throttle = merged_throttle(
        existing.and_then(|config| config.power_limit_throttle),
        args,
        lowest_cap,
    )?;

    print_throttle_settings(&throttle);

    ctx.edit_gpu_config(id, move |config| {
        config.power_limit_throttle = Some(throttle);
    })
    .await?;
    println!("Enabled power limit throttling");

    Ok(())
}

/// Combines the settings which are already configured with the provided arguments.
fn merged_throttle(
    existing: Option<PowerLimitThrottle>,
    args: &PowerThrottleSetArgs,
    lowest_cap: f64,
) -> Result<PowerLimitThrottle> {
    let mut throttle = existing.unwrap_or(PowerLimitThrottle {
        min_power: lowest_cap,
        thresholds: Default::default(),
        step: default_power_throttle_step(),
        interval_secs: default_power_throttle_interval_secs(),
        hysteresis: default_power_throttle_hysteresis(),
        load_lock: None,
    });

    if let Some(min_power) = args.min_power {
        throttle.min_power = min_power;
    }
    if let Some(step) = args.step {
        if step <= 0.0 {
            bail!("Power limit step has to be greater than 0");
        }
        throttle.step = step;
    }
    if let Some(interval_secs) = args.interval_secs {
        throttle.interval_secs = interval_secs;
    }
    if let Some(hysteresis) = args.hysteresis {
        throttle.hysteresis = hysteresis;
    }

    for (sensor, temperature) in parse_thresholds(&args.threshold)? {
        throttle.thresholds.insert(sensor, temperature);
    }

    if throttle.thresholds.is_empty() {
        bail!(
            "No temperature thresholds configured, add at least one with `--threshold sensor=degrees`"
        );
    }
    if throttle.min_power < lowest_cap {
        eprintln!(
            "Warning: the minimum power limit {}W is lower than the lowest power limit of this GPU ({lowest_cap}W), it will be clamped",
            throttle.min_power
        );
    }

    if args.disable_load_lock {
        throttle.load_lock = None;
    } else if args.load_lock_pct.is_some() || args.load_lock_duration_secs.is_some() {
        let mut load_lock = throttle.load_lock.unwrap_or(PowerThrottleLoadLock {
            power_pct: default_load_lock_power_pct(),
            duration_secs: default_load_lock_duration_secs(),
        });
        if let Some(power_pct) = args.load_lock_pct {
            load_lock.power_pct = power_pct;
        }
        if let Some(duration_secs) = args.load_lock_duration_secs {
            load_lock.duration_secs = duration_secs;
        }
        throttle.load_lock = Some(load_lock);
    }

    Ok(throttle)
}

fn parse_thresholds(values: &[String]) -> Result<Vec<(String, u8)>> {
    values
        .iter()
        .map(|value| {
            let (sensor, temperature) = value.split_once('=').with_context(|| {
                format!("Invalid threshold '{value}', expected the `sensor=degrees` format")
            })?;
            let temperature = temperature.trim().parse::<u8>().with_context(|| {
                format!("Invalid temperature '{temperature}' for sensor '{sensor}'")
            })?;

            Ok((sensor.trim().to_owned(), temperature))
        })
        .collect()
}

fn print_throttle_settings(throttle: &PowerLimitThrottle) {
    println!("  Minimum power limit: {}W", throttle.min_power);
    println!(
        "  Step: {}W, check interval: {}s, hysteresis: {}",
        throttle.step, throttle.interval_secs, throttle.hysteresis
    );

    let thresholds = throttle
        .thresholds
        .iter()
        .map(|(name, temperature)| format!("{name}: {temperature}"))
        .collect::<Vec<String>>()
        .join(", ");
    println!("  Temperature thresholds: {thresholds}");

    match &throttle.load_lock {
        Some(load_lock) => println!(
            "  Load lock: at {}% of the power limit for {}s",
            load_lock.power_pct, load_lock.duration_secs
        ),
        None => println!("  Load lock: disabled"),
    }
}

pub async fn list_profiles(_: &ProfileArgs, ctx: CliContext<'_>) -> Result<()> {
    let profiles_info = ctx.client.list_profiles(false).await?;
    println!("{}", PROFILE_DEFAULT);
    for (name, _rule) in profiles_info.profiles {
        println!("{}", name);
    }
    Ok(())
}

pub async fn current_profile(_: &ProfileArgs, ctx: CliContext<'_>) -> Result<()> {
    let profiles_info = ctx.client.list_profiles(false).await?;
    if let Some(current_profile) = profiles_info.current_profile {
        println!("{}", current_profile);
    } else {
        println!("{}", PROFILE_DEFAULT);
    }
    Ok(())
}

pub async fn set_profile(args: &SetProfileArgs, ctx: CliContext<'_>) -> Result<()> {
    let new_profile = args.name.trim();

    if new_profile.to_lowercase() == PROFILE_DEFAULT.to_lowercase() {
        ctx.client.set_profile(None, false).await?;
        println!("{}", PROFILE_DEFAULT);
    } else {
        ctx.client
            .set_profile(Some(new_profile.to_string()), false)
            .await?;
        println!("{}", new_profile);
    }
    Ok(())
}

pub async fn current_auto_switch(_: &ProfileAutoSwitchArgs, ctx: CliContext<'_>) -> Result<()> {
    let auto_switch = ctx.client.list_profiles(false).await?.auto_switch;
    if auto_switch {
        println!("enabled");
    } else {
        println!("disabled");
    }
    Ok(())
}

pub async fn set_auto_switch(
    _: &ProfileAutoSwitchArgs,
    ctx: CliContext<'_>,
    enable: bool,
) -> Result<()> {
    ctx.client.set_profile(None, enable).await?;
    if enable {
        println!("enabled");
    } else {
        println!("disabled");
    }
    Ok(())
}

pub async fn detach(ctx: CliContext<'_>) -> Result<()> {
    let id = ctx
        .args
        .gpu_id
        .as_deref()
        .context("`--gpu-id` must be passed explicitly for reattaching")?;
    let name = ctx.name_for_id(id).await?;
    ctx.client.detach(id).await?;

    if let Some(name) = name {
        println!("Detached GPU '{id}' ({name})");
    } else {
        println!("Detached GPU '{id}'");
    }

    Ok(())
}

pub async fn reattach(ctx: CliContext<'_>) -> Result<()> {
    let id = ctx
        .args
        .gpu_id
        .as_deref()
        .context("`--gpu-id` must be passed explicitly for reattaching")?;
    ctx.client.reattach(id).await?;

    let name = ctx.name_for_id(id).await?;

    if let Some(name) = name {
        println!("Reattached GPU '{id}' ({name})");
    } else {
        println!("Reattached GPU '{id}'");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PowerThrottleSetArgs, merged_throttle, parse_thresholds};

    #[test]
    fn parses_thresholds() {
        let values = ["junction=92".to_owned(), " mem =95 ".to_owned()];
        assert_eq!(
            parse_thresholds(&values).unwrap(),
            vec![("junction".to_owned(), 92), ("mem".to_owned(), 95)]
        );

        assert!(parse_thresholds(&["junction".to_owned()]).is_err());
        assert!(parse_thresholds(&["junction=very-hot".to_owned()]).is_err());
    }

    #[test]
    fn creates_throttle_settings_from_defaults() {
        let args = PowerThrottleSetArgs {
            threshold: vec!["junction=92".to_owned()],
            ..PowerThrottleSetArgs::default()
        };

        let throttle = merged_throttle(None, &args, 261.0).unwrap();
        assert_eq!(throttle.min_power, 261.0);
        assert_eq!(*throttle.thresholds.get("junction").unwrap(), 92);
        assert_eq!(throttle.load_lock, None);
    }

    #[test]
    fn keeps_existing_throttle_settings() {
        let args = PowerThrottleSetArgs {
            threshold: vec!["junction=92".to_owned()],
            load_lock_pct: Some(80),
            ..PowerThrottleSetArgs::default()
        };
        let mut throttle = merged_throttle(None, &args, 261.0).unwrap();
        assert_eq!(throttle.load_lock.unwrap().power_pct, 80);

        let args = PowerThrottleSetArgs {
            step: Some(5.0),
            threshold: vec!["mem=95".to_owned()],
            disable_load_lock: true,
            ..PowerThrottleSetArgs::default()
        };
        throttle = merged_throttle(Some(throttle), &args, 261.0).unwrap();

        assert_eq!(throttle.step, 5.0);
        assert_eq!(*throttle.thresholds.get("junction").unwrap(), 92);
        assert_eq!(*throttle.thresholds.get("mem").unwrap(), 95);
        assert_eq!(throttle.load_lock, None);
    }

    #[test]
    fn requires_at_least_one_threshold() {
        let args = PowerThrottleSetArgs {
            min_power: Some(100.0),
            ..PowerThrottleSetArgs::default()
        };

        let result = merged_throttle(None, &args, 100.0);
        assert!(result.is_err());
    }
}

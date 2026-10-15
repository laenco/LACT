use crate::{
    APP_BROKER, I18N,
    app::{
        components::page_section::PageSection, msg::AppMsg, utils::formatting::fmt_value_with_unit,
    },
};
use adw::prelude::*;
use gtk::glib;
use i18n_embed_fl::fl;
use indexmap::IndexMap;
use lact_schema::{
    PowerLimitThrottleStatus,
    config::{
        GpuConfig, PowerLimitThrottle, PowerThrottleLoadLock, default_load_lock_duration_secs,
        default_load_lock_power_pct, default_power_throttle_hysteresis,
        default_power_throttle_interval_secs, default_power_throttle_step,
    },
};
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, css};

/// A threshold of zero means that the sensor is not used.
const MAX_THRESHOLD: f64 = 150.0;
const MIN_STEP: f64 = 1.0;
const MAX_STEP: f64 = 100.0;
const MAX_HYSTERESIS: f64 = 30.0;
const MIN_INTERVAL_SECS: f64 = 1.0;
const MAX_INTERVAL_SECS: f64 = 3_600.0;
const MIN_LOAD_LOCK_PCT: f64 = 1.0;
const MAX_LOAD_LOCK_PCT: f64 = 100.0;
const MAX_LOAD_LOCK_SECS: f64 = 86_400.0;

/// Settings of the automatic power limit throttling.
///
/// The values are kept in the model as they would be written to the configuration,
/// so that applying the settings does not depend on the state of the widgets.
pub struct PowerThrottleDialog {
    /// Whether the power limit of the device can be changed at all.
    supported: bool,
    min_cap: f64,
    max_cap: f64,
    /// Live state reported by the daemon, when the throttling is running.
    status: Option<PowerLimitThrottleStatus>,
    /// Temperature sensors which have a row in the dialog: the ones which can be used
    /// for the thresholds, and the ones present in the configuration.
    sensors: Vec<String>,
    /// Sensors which the device can currently use for the thresholds.
    reported_sensors: Vec<String>,

    enabled: bool,
    thresholds: IndexMap<String, u8>,
    min_power: f64,
    step: f64,
    hysteresis: u8,
    interval_secs: u64,
    load_lock_enabled: bool,
    load_lock_power_pct: u8,
    load_lock_duration_secs: u64,

    /// The settings which were loaded from the daemon, used to tell a repeated
    /// update of the same configuration apart from a configuration changed elsewhere.
    loaded_settings: Option<PowerLimitThrottle>,
    /// Whether the settings were changed since they were loaded from the daemon.
    edited: bool,

    /// Rows created for each temperature sensor, kept to be able to remove them.
    threshold_rows: Vec<(String, gtk::ListBoxRow)>,
}

#[derive(Debug)]
pub enum PowerThrottleDialogMsg {
    Show {
        parent: gtk::Widget,
    },
    /// The settings currently present in the configuration of the device.
    Config(Option<PowerLimitThrottle>),
    /// Power limit range and temperature sensors of the device.
    Capabilities {
        min_cap: f64,
        max_cap: f64,
        sensors: Vec<String>,
    },
    Status(Option<PowerLimitThrottleStatus>),
    EnabledChanged(bool),
    ThresholdChanged {
        sensor: String,
        value: u8,
    },
    MinPowerChanged(f64),
    StepChanged(f64),
    HysteresisChanged(u8),
    IntervalChanged(u64),
    LoadLockChanged(bool),
    LoadLockPowerPctChanged(u8),
    LoadLockDurationChanged(u64),
}

#[relm4::component(pub)]
impl relm4::Component for PowerThrottleDialog {
    type Init = ();
    type Input = PowerThrottleDialogMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::Dialog {
            set_content_width: 460,
            set_follows_content_size: true,
            set_title: &fl!(I18N, "power-throttle-dialog-title"),

            #[wrap(Some)]
            set_child = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {},

                #[wrap(Some)]
                set_content = &gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_propagate_natural_height: true,
                    set_max_content_height: 600,

                    PageSection {
                        set_hide_visible_container: true,
                        set_margin_all: 15,

                        append_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 10,
                            #[watch]
                            set_visible: model.status.is_some(),

                            gtk::ListBox {
                                add_css_class: css::BOXED_LIST,
                                set_selection_mode: gtk::SelectionMode::None,

                                gtk::ListBoxRow {
                                    set_activatable: false,
                                    set_selectable: false,

                                    gtk::Label {
                                        set_xalign: 0.0,
                                        set_wrap: true,
                                        #[watch]
                                        set_label: &model.status_text(),
                                    },
                                },
                            },
                        },

                        append_child = &gtk::ListBox {
                            add_css_class: css::BOXED_LIST,
                            set_selection_mode: gtk::SelectionMode::None,

                            gtk::ListBoxRow {
                                set_activatable: false,
                                set_selectable: false,

                                gtk::Box {
                                    set_spacing: 10,

                                    gtk::Label {
                                        set_label: &fl!(I18N, "power-throttle-enabled"),
                                        set_xalign: 0.0,
                                        set_hexpand: true,
                                        set_valign: gtk::Align::Center,
                                    },

                                    #[name = "enabled_switch"]
                                    gtk::Switch {
                                        // The state is only pushed to the widget when a configuration
                                        // is loaded, so that an unrelated update cannot revert a value
                                        // which the user is changing.
                                        connect_state_notify[sender] => move |switch| {
                                            sender.input(PowerThrottleDialogMsg::EnabledChanged(switch.is_active()));
                                        } @ enabled_switch_signal,
                                    },
                                },
                            },

                            gtk::ListBoxRow {
                                set_activatable: false,
                                set_selectable: false,

                                gtk::Label {
                                    set_label: &fl!(I18N, "power-throttle-no-thresholds"),
                                    set_xalign: 0.0,
                                    add_css_class: css::CAPTION,
                                    #[watch]
                                    set_visible: model.enabled && !model.has_thresholds(),
                                },
                            },
                        },

                        append_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 5,

                            gtk::Label {
                                set_label: &fl!(I18N, "power-throttle-thresholds"),
                                set_halign: gtk::Align::Start,
                                add_css_class: css::CAPTION,
                                add_css_class: css::DIM_LABEL,
                            },

                            gtk::Label {
                                set_label: &fl!(I18N, "power-throttle-thresholds-description"),
                                set_xalign: 0.0,
                                set_wrap: true,
                                add_css_class: css::CAPTION,
                                add_css_class: css::DIM_LABEL,
                            },

                            #[name = "thresholds_list"]
                            gtk::ListBox {
                                add_css_class: css::BOXED_LIST,
                                set_selection_mode: gtk::SelectionMode::None,
                            },

                            gtk::Label {
                                set_xalign: 0.0,
                                set_wrap: true,
                                add_css_class: css::CAPTION,
                                add_css_class: css::WARNING,
                                #[watch]
                                set_label: &model.unavailable_text(),
                                #[watch]
                                set_visible: !model.unavailable_sensors().is_empty(),
                            },
                        },

                        append_child = &gtk::ListBox {
                            add_css_class: css::BOXED_LIST,
                            set_selection_mode: gtk::SelectionMode::None,

                            #[template]
                            #[name = "min_power_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-min-power"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (model.min_cap, model.max_cap),
                                    set_increments: (1.0, 10.0),
                                    set_value: model.min_power,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::MinPowerChanged(spin.value()));
                                    } @ min_power_signal,
                                },
                            },

                            #[template]
                            #[name = "step_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-step"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (MIN_STEP, MAX_STEP),
                                    set_increments: (1.0, 5.0),
                                    set_value: model.step,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::StepChanged(spin.value()));
                                    } @ step_signal,
                                },
                            },

                            #[template]
                            #[name = "hysteresis_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-hysteresis"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (0.0, MAX_HYSTERESIS),
                                    set_increments: (1.0, 5.0),
                                    set_value: model.hysteresis as f64,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::HysteresisChanged(spin.value() as u8));
                                    } @ hysteresis_signal,
                                },
                            },

                            #[template]
                            #[name = "interval_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-interval"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (MIN_INTERVAL_SECS, MAX_INTERVAL_SECS),
                                    set_increments: (1.0, 10.0),
                                    set_value: model.interval_secs as f64,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::IntervalChanged(spin.value() as u64));
                                    } @ interval_signal,
                                },
                            },
                        },

                        append_child = &gtk::ListBox {
                            add_css_class: css::BOXED_LIST,
                            set_selection_mode: gtk::SelectionMode::None,

                            gtk::ListBoxRow {
                                set_activatable: false,
                                set_selectable: false,

                                gtk::Box {
                                    set_spacing: 10,

                                    gtk::Label {
                                        set_label: &fl!(I18N, "power-throttle-load-lock"),
                                        set_tooltip_text: Some(&fl!(I18N, "power-throttle-load-lock-description")),
                                        set_xalign: 0.0,
                                        set_hexpand: true,
                                        set_valign: gtk::Align::Center,
                                    },

                                    #[name = "load_lock_switch"]
                                    gtk::Switch {
                                        connect_state_notify[sender] => move |switch| {
                                            sender.input(PowerThrottleDialogMsg::LoadLockChanged(switch.is_active()));
                                        } @ load_lock_switch_signal,
                                    },
                                },
                            },

                            #[template]
                            #[name = "load_lock_power_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-load-lock-power"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (MIN_LOAD_LOCK_PCT, MAX_LOAD_LOCK_PCT),
                                    set_increments: (1.0, 10.0),
                                    set_value: model.load_lock_power_pct as f64,
                                    #[watch]
                                    set_sensitive: model.load_lock_enabled,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::LoadLockPowerPctChanged(spin.value() as u8));
                                    } @ load_lock_power_signal,
                                },
                            },

                            #[template]
                            #[name = "load_lock_duration_row"]
                            SettingRow {
                                #[template_child]
                                label {
                                    set_label: &fl!(I18N, "power-throttle-load-lock-duration"),
                                },

                                #[template_child]
                                spinbutton {
                                    set_range: (0.0, MAX_LOAD_LOCK_SECS),
                                    set_increments: (5.0, 60.0),
                                    set_value: model.load_lock_duration_secs as f64,
                                    #[watch]
                                    set_sensitive: model.load_lock_enabled,
                                    connect_value_changed[sender] => move |spin| {
                                        sender.input(PowerThrottleDialogMsg::LoadLockDurationChanged(spin.value() as u64));
                                    } @ load_lock_duration_signal,
                                },
                            },
                        },
                    },
                },
            },
        },
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Self {
            supported: false,
            min_cap: 0.0,
            max_cap: 0.0,
            status: None,
            sensors: Vec::new(),
            reported_sensors: Vec::new(),
            enabled: false,
            thresholds: IndexMap::new(),
            min_power: 0.0,
            step: default_power_throttle_step(),
            hysteresis: default_power_throttle_hysteresis(),
            interval_secs: default_power_throttle_interval_secs(),
            load_lock_enabled: false,
            load_lock_power_pct: default_load_lock_power_pct(),
            load_lock_duration_secs: default_load_lock_duration_secs(),
            loaded_settings: None,
            edited: false,
            threshold_rows: Vec::new(),
        };

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    #[allow(clippy::too_many_lines)]
    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        // Everything which does not come from the daemon is an edit made by the user.
        if !matches!(
            msg,
            PowerThrottleDialogMsg::Config(_)
                | PowerThrottleDialogMsg::Capabilities { .. }
                | PowerThrottleDialogMsg::Status(_)
                | PowerThrottleDialogMsg::Show { .. }
        ) {
            self.edited = true;
        }

        match msg {
            PowerThrottleDialogMsg::Show { parent } => {
                self.update_view(widgets, sender.clone());
                root.present(Some(&parent));
            }
            PowerThrottleDialogMsg::Config(settings) => {
                // The configuration is fetched periodically, and it should not discard
                // the changes which the user is making right now. A configuration which
                // has changed elsewhere (another LACT instance, a profile switch) is loaded.
                if !self.edited || settings != self.loaded_settings {
                    self.edited = false;
                    self.loaded_settings = settings.clone();
                    self.load_settings(settings);
                    self.set_spin_values(widgets);
                    self.set_switch_states(widgets);
                }
                // Sensors which are present in the configuration are shown as well.
                self.sensors = self.visible_sensors(self.reported_sensors.clone());
                self.rebuild_threshold_rows(widgets, &sender);
            }
            PowerThrottleDialogMsg::Capabilities {
                min_cap,
                max_cap,
                sensors,
            } => {
                self.supported = min_cap > 0.0 && max_cap > 0.0;

                if self.min_cap != min_cap || self.max_cap != max_cap {
                    self.min_cap = min_cap;
                    self.max_cap = max_cap;

                    // The lowest limit defaults to the lowest limit of the device.
                    if self.min_power == 0.0 {
                        self.min_power = min_cap;
                    }
                    self.set_min_power_range(widgets, min_cap, max_cap);
                }

                self.reported_sensors = sensors.clone();
                let sensors = self.visible_sensors(sensors);
                if self.sensors != sensors {
                    self.sensors = sensors;
                    self.rebuild_threshold_rows(widgets, &sender);
                }
            }
            PowerThrottleDialogMsg::Status(status) => {
                self.status = status;
            }
            PowerThrottleDialogMsg::EnabledChanged(enabled) => {
                // The switch is also set from the model, which must not count as an edit.
                if self.enabled != enabled {
                    self.enabled = enabled;
                    APP_BROKER.send(AppMsg::SettingsChanged);
                }
            }
            PowerThrottleDialogMsg::ThresholdChanged { sensor, value } => {
                self.thresholds.insert(sensor, value);
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::MinPowerChanged(value) => {
                self.min_power = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::StepChanged(value) => {
                self.step = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::HysteresisChanged(value) => {
                self.hysteresis = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::IntervalChanged(value) => {
                self.interval_secs = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::LoadLockChanged(enabled) => {
                if self.load_lock_enabled != enabled {
                    self.load_lock_enabled = enabled;
                    APP_BROKER.send(AppMsg::SettingsChanged);
                }
            }
            PowerThrottleDialogMsg::LoadLockPowerPctChanged(value) => {
                self.load_lock_power_pct = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
            PowerThrottleDialogMsg::LoadLockDurationChanged(value) => {
                self.load_lock_duration_secs = value;
                APP_BROKER.send(AppMsg::SettingsChanged);
            }
        }

        self.update_view(widgets, sender);
    }
}

impl PowerThrottleDialog {
    /// Writes the settings of the throttling into the configuration of a device.
    ///
    /// The section is left as is on devices which do not allow changing the power limit,
    /// so that a configuration made elsewhere is not wiped by accident.
    pub fn apply_config(&self, config: &mut GpuConfig) {
        if !self.supported {
            return;
        }
        config.power_limit_throttle = self.build_settings();
    }

    fn build_settings(&self) -> Option<PowerLimitThrottle> {
        if !self.enabled {
            return None;
        }

        let thresholds: IndexMap<String, u8> = self
            .thresholds
            .iter()
            .filter(|(_, temperature)| **temperature > 0)
            .map(|(sensor, temperature)| (sensor.clone(), *temperature))
            .collect();
        // The throttling has nothing to react to without any threshold.
        if thresholds.is_empty() {
            return None;
        }

        Some(PowerLimitThrottle {
            min_power: self.min_power,
            thresholds,
            step: self.step.max(MIN_STEP),
            interval_secs: self.interval_secs.max(MIN_INTERVAL_SECS as u64),
            hysteresis: self.hysteresis,
            load_lock: self.load_lock(),
        })
    }

    fn load_lock(&self) -> Option<PowerThrottleLoadLock> {
        self.load_lock_enabled.then_some(PowerThrottleLoadLock {
            power_pct: self.load_lock_power_pct,
            duration_secs: self.load_lock_duration_secs,
        })
    }

    /// Replaces the values in the model with the ones from the configuration.
    fn load_settings(&mut self, settings: Option<PowerLimitThrottle>) {
        let Some(settings) = settings else {
            self.enabled = false;
            self.thresholds.clear();
            self.load_lock_enabled = false;
            return;
        };

        self.enabled = true;
        self.min_power = settings.min_power;
        self.thresholds = settings.thresholds;
        self.step = settings.step;
        self.hysteresis = settings.hysteresis;
        self.interval_secs = settings.interval_secs;

        match settings.load_lock {
            Some(lock) => {
                self.load_lock_enabled = true;
                self.load_lock_power_pct = lock.power_pct;
                self.load_lock_duration_secs = lock.duration_secs;
            }
            None => self.load_lock_enabled = false,
        }
    }

    /// Sensors to show a threshold for: the ones reported by the device, and any sensor
    /// which is present in the configuration already.
    fn visible_sensors(&self, mut sensors: Vec<String>) -> Vec<String> {
        for sensor in self.thresholds.keys() {
            if !sensors.contains(sensor) {
                sensors.push(sensor.clone());
            }
        }
        sensors
    }

    fn has_thresholds(&self) -> bool {
        self.thresholds.values().any(|temperature| *temperature > 0)
    }

    /// Whether a sensor cannot be used as a threshold by the daemon.
    ///
    /// Nothing is known about the sensors of the device until its statistics have arrived,
    /// so no sensor is treated as unavailable before that.
    fn is_unavailable(&self, sensor: &str) -> bool {
        !self.reported_sensors.is_empty()
            && !self.reported_sensors.iter().any(|name| name == sensor)
    }

    /// Sensors used in the settings which cannot be used on this device.
    fn unavailable_sensors(&self) -> Vec<&str> {
        self.thresholds
            .iter()
            .filter(|(_, temperature)| **temperature > 0)
            .map(|(sensor, _)| sensor.as_str())
            .filter(|sensor| self.is_unavailable(sensor))
            .collect()
    }

    /// Explains which of the used sensors cannot be used, if there are any.
    fn unavailable_text(&self) -> String {
        let unavailable = self.unavailable_sensors();
        if unavailable.is_empty() {
            return String::new();
        }

        fl!(
            I18N,
            "power-throttle-unavailable-sensors",
            sensors = unavailable.join(", "),
        )
    }

    /// Applies the allowed range of the lowest limit, which may clamp its value.
    fn set_min_power_range(
        &self,
        widgets: &PowerThrottleDialogWidgets,
        min_cap: f64,
        max_cap: f64,
    ) {
        let spinbutton = &widgets.min_power_row.spinbutton;
        spinbutton.block_signal(&widgets.min_power_signal);
        spinbutton.set_range(min_cap, max_cap);
        spinbutton.set_value(self.min_power.clamp(min_cap, max_cap));
        spinbutton.unblock_signal(&widgets.min_power_signal);
    }

    fn set_spin_values(&self, widgets: &PowerThrottleDialogWidgets) {
        let min_power = self.min_power.clamp(self.min_cap, self.max_cap);
        set_spin_value(
            &widgets.min_power_row.spinbutton,
            &widgets.min_power_signal,
            min_power,
        );
        set_spin_value(
            &widgets.step_row.spinbutton,
            &widgets.step_signal,
            self.step,
        );
        set_spin_value(
            &widgets.hysteresis_row.spinbutton,
            &widgets.hysteresis_signal,
            f64::from(self.hysteresis),
        );
        set_spin_value(
            &widgets.interval_row.spinbutton,
            &widgets.interval_signal,
            self.interval_secs as f64,
        );
        set_spin_value(
            &widgets.load_lock_power_row.spinbutton,
            &widgets.load_lock_power_signal,
            f64::from(self.load_lock_power_pct),
        );
        set_spin_value(
            &widgets.load_lock_duration_row.spinbutton,
            &widgets.load_lock_duration_signal,
            self.load_lock_duration_secs as f64,
        );
    }

    /// Pushes the settings loaded from a configuration to the switches, without
    /// treating them as changes made by the user.
    fn set_switch_states(&self, widgets: &PowerThrottleDialogWidgets) {
        set_switch_state(
            &widgets.enabled_switch,
            &widgets.enabled_switch_signal,
            self.enabled,
        );
        set_switch_state(
            &widgets.load_lock_switch,
            &widgets.load_lock_switch_signal,
            self.load_lock_enabled,
        );
    }

    fn rebuild_threshold_rows(
        &mut self,
        widgets: &PowerThrottleDialogWidgets,
        sender: &ComponentSender<Self>,
    ) {
        for (_, row) in self.threshold_rows.drain(..) {
            widgets.thresholds_list.remove(&row);
        }

        for sensor in &self.sensors {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);

            let content = gtk::Box::builder().spacing(10).build();

            let label = gtk::Label::builder()
                .label(sensor.as_str())
                .xalign(0.0)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();

            // A sensor taken from the configuration which cannot be used by the device is
            // shown to make it possible to turn it off, but not as a usable option.
            if self.is_unavailable(sensor) {
                label.add_css_class(css::WARNING);
                label.set_tooltip_text(Some(&fl!(I18N, "power-throttle-sensor-unavailable")));
            }

            let spin = gtk::SpinButton::with_range(0.0, MAX_THRESHOLD, 1.0);
            spin.set_digits(0);
            spin.set_numeric(true);
            spin.set_width_chars(6);
            spin.set_max_width_chars(6);
            spin.set_valign(gtk::Align::Center);
            // The value is set before the signal is connected to avoid an edit notification.
            spin.set_value(f64::from(
                self.thresholds.get(sensor).copied().unwrap_or_default(),
            ));

            let sensor_name = sensor.clone();
            let change_sender = sender.clone();
            spin.connect_value_changed(move |spin| {
                change_sender.input(PowerThrottleDialogMsg::ThresholdChanged {
                    sensor: sensor_name.clone(),
                    value: spin.value() as u8,
                });
            });

            content.append(&label);
            content.append(&spin);
            row.set_child(Some(&content));

            widgets.thresholds_list.append(&row);
            self.threshold_rows.push((sensor.clone(), row));
        }
    }

    fn status_text(&self) -> String {
        let Some(status) = self.status.as_ref() else {
            return String::new();
        };

        let mut lines = vec![power_throttle_status_line(status)];

        if !status.breached.is_empty() {
            lines.push(fl!(
                I18N,
                "power-throttle-status-over-threshold",
                sensors = status.breached.join(", "),
            ));
        }

        if status.load_locked {
            lines.push(fl!(I18N, "power-throttle-status-load-locked"));
        }

        lines.join("\n")
    }
}

/// Describes the power limit which is currently in effect.
pub fn power_throttle_status_line(status: &PowerLimitThrottleStatus) -> String {
    let watt = fl!(I18N, "watt");
    let base = fmt_value_with_unit(status.base_cap, &watt);

    if status.effective_cap + 0.001 < status.base_cap {
        fl!(
            I18N,
            "power-throttle-status-lowered",
            cap = fmt_value_with_unit(status.effective_cap, &watt),
            base = base,
        )
    } else {
        fl!(I18N, "power-throttle-status-active", base = base)
    }
}

/// Sets a value without treating it as an edit made by the user.
fn set_spin_value(spin: &gtk::SpinButton, signal: &glib::SignalHandlerId, value: f64) {
    spin.block_signal(signal);
    spin.set_value(value);
    spin.unblock_signal(signal);
}

/// Sets a switch state without treating it as an edit made by the user.
fn set_switch_state(switch: &gtk::Switch, signal: &glib::SignalHandlerId, value: bool) {
    switch.block_signal(signal);
    switch.set_active(value);
    switch.unblock_signal(signal);
}

#[relm4::widget_template(pub)]
impl relm4::WidgetTemplate for SettingRow {
    view! {
        gtk::ListBoxRow {
            set_activatable: false,
            set_selectable: false,

            gtk::Box {
                set_spacing: 10,

                #[name = "label"]
                gtk::Label {
                    set_xalign: 0.0,
                    set_hexpand: true,
                    set_valign: gtk::Align::Center,
                },

                #[name = "spinbutton"]
                gtk::SpinButton {
                    set_digits: 0,
                    set_numeric: true,
                    set_width_chars: 8,
                    set_max_width_chars: 8,
                    set_valign: gtk::Align::Center,
                },
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> PowerThrottleDialog {
        PowerThrottleDialog {
            supported: true,
            min_cap: 50.0,
            max_cap: 300.0,
            status: None,
            sensors: vec!["junction".to_owned(), "mem".to_owned()],
            reported_sensors: vec!["junction".to_owned(), "mem".to_owned()],
            enabled: false,
            thresholds: IndexMap::new(),
            min_power: 50.0,
            step: default_power_throttle_step(),
            hysteresis: default_power_throttle_hysteresis(),
            interval_secs: default_power_throttle_interval_secs(),
            load_lock_enabled: false,
            load_lock_power_pct: default_load_lock_power_pct(),
            load_lock_duration_secs: default_load_lock_duration_secs(),
            loaded_settings: None,
            edited: false,
            threshold_rows: Vec::new(),
        }
    }

    fn settings() -> PowerLimitThrottle {
        PowerLimitThrottle {
            min_power: 120.0,
            thresholds: [("junction".to_owned(), 95)].into(),
            step: 15.0,
            interval_secs: 5,
            hysteresis: 3,
            load_lock: Some(PowerThrottleLoadLock {
                power_pct: 85,
                duration_secs: 60,
            }),
        }
    }

    #[test]
    fn unchanged_dialog_keeps_the_settings() {
        let mut model = model();
        model.load_settings(Some(settings()));
        assert_eq!(model.build_settings(), Some(settings()));
    }

    #[test]
    fn disabled_throttle_clears_the_section() {
        let mut model = model();
        model.load_settings(Some(settings()));
        model.load_settings(None);
        assert!(!model.enabled);
        assert_eq!(model.build_settings(), None);

        let mut config = GpuConfig {
            power_limit_throttle: Some(settings()),
            ..Default::default()
        };
        model.apply_config(&mut config);
        assert_eq!(config.power_limit_throttle, None);
    }

    #[test]
    fn sensors_without_a_threshold_are_skipped() {
        let mut model = model();
        model.enabled = true;
        model.thresholds = [("junction".to_owned(), 0), ("mem".to_owned(), 92)].into();
        let built = model.build_settings().expect("has a threshold");
        assert_eq!(built.thresholds.len(), 1);
        assert_eq!(built.thresholds["mem"], 92);
    }

    #[test]
    fn without_any_threshold_nothing_is_enabled() {
        let mut model = model();
        model.enabled = true;
        model.thresholds = [("junction".to_owned(), 0)].into();
        assert!(!model.has_thresholds());
        assert_eq!(model.build_settings(), None);
    }

    #[test]
    fn configured_sensors_stay_visible() {
        let mut model = model();
        model.thresholds = [("edge".to_owned(), 90)].into();
        let sensors = model.visible_sensors(vec!["junction".to_owned()]);
        assert_eq!(sensors, ["junction", "edge"].map(str::to_owned));
    }

    #[test]
    fn sensors_which_the_device_cannot_use_are_reported() {
        let mut model = model();
        model.reported_sensors = vec!["junction".to_owned()];
        model.thresholds = [
            ("junction".to_owned(), 95),
            ("vrgfx".to_owned(), 90),
            ("vrmem".to_owned(), 90),
        ]
        .into();

        assert_eq!(model.unavailable_sensors(), ["vrgfx", "vrmem"]);
        let text = model.unavailable_text();
        assert!(text.contains("vrgfx"), "unexpected text: {text}");
        assert!(text.contains("vrmem"), "unexpected text: {text}");
    }

    #[test]
    fn sensors_are_usable_before_the_device_is_known() {
        let mut model = model();
        model.reported_sensors = Vec::new();
        model.thresholds = [("vrgfx".to_owned(), 90)].into();
        assert!(model.unavailable_sensors().is_empty());
        assert!(model.unavailable_text().is_empty());
    }

    #[test]
    fn unused_sensors_are_not_reported_as_unavailable() {
        let mut model = model();
        model.reported_sensors = vec!["junction".to_owned()];
        model.thresholds = [("vrgfx".to_owned(), 0)].into();
        assert!(model.unavailable_sensors().is_empty());
        assert!(!model.is_unavailable("junction"));
    }
}

#[cfg(all(test, feature = "gtk-tests"))]
mod gtk_tests {
    use super::*;
    use crate::app::utils::ext::RelmDefaultLauchable;
    use relm4::ComponentController;

    #[test]
    #[ignore = "requires a GTK display; run explicitly with --ignored"]
    fn shows_a_row_for_every_known_sensor() {
        adw::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let _guard = context.acquire().unwrap();

        let dialog = PowerThrottleDialog::detach_default();
        dialog.emit(PowerThrottleDialogMsg::Capabilities {
            min_cap: 100.0,
            max_cap: 350.0,
            sensors: vec!["junction".to_owned(), "mem".to_owned()],
        });
        dialog.emit(PowerThrottleDialogMsg::Config(Some(PowerLimitThrottle {
            min_power: 150.0,
            thresholds: [("edge".to_owned(), 85)].into(),
            step: 10.0,
            interval_secs: 2,
            hysteresis: 5,
            load_lock: None,
        })));
        while context.pending() {
            context.iteration(false);
        }

        {
            let model = dialog.model();
            // The sensors of the device, and the one taken from the configuration.
            assert_eq!(model.threshold_rows.len(), 3);
            assert!(model.enabled);
            assert_eq!(model.build_settings().map(|s| s.min_power), Some(150.0));

            // The sensor taken from the configuration cannot be used by the device.
            assert_eq!(model.unavailable_sensors(), ["edge"]);
            assert!(
                model.unavailable_text().contains("edge"),
                "unexpected warning: {}",
                model.unavailable_text()
            );
        }

        dialog.emit(PowerThrottleDialogMsg::Status(Some(
            PowerLimitThrottleStatus {
                base_cap: 300.0,
                effective_cap: 260.0,
                load_locked: true,
                breached: vec!["junction".to_owned()],
            },
        )));
        while context.pending() {
            context.iteration(false);
        }
        let status = dialog.model().status_text();
        assert!(status.contains("junction"), "unexpected status: {status}");
    }

    /// A switch that was changed by the user has to survive the periodic updates
    /// which the dialog receives while it is open.
    #[test]
    #[ignore = "requires a GTK display; run explicitly with --ignored"]
    fn switch_changed_by_the_user_is_not_reverted() {
        adw::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let _guard = context.acquire().unwrap();

        let dialog = PowerThrottleDialog::detach_default();
        dialog.emit(PowerThrottleDialogMsg::Capabilities {
            min_cap: 100.0,
            max_cap: 350.0,
            sensors: vec!["junction".to_owned()],
        });
        while context.pending() {
            context.iteration(false);
        }

        dialog.widgets().load_lock_switch.set_active(true);
        while context.pending() {
            context.iteration(false);
        }
        assert!(dialog.model().load_lock_enabled);

        dialog.emit(PowerThrottleDialogMsg::Capabilities {
            min_cap: 100.0,
            max_cap: 350.0,
            sensors: vec!["junction".to_owned()],
        });
        dialog.emit(PowerThrottleDialogMsg::Status(None));
        while context.pending() {
            context.iteration(false);
        }

        assert!(dialog.model().load_lock_enabled);
        assert!(dialog.widgets().load_lock_switch.is_active());
    }

    /// A configuration which is fetched again must not discard the changes that are
    /// being made in the dialog, but a configuration changed elsewhere must be loaded.
    #[test]
    #[ignore = "requires a GTK display; run explicitly with --ignored"]
    fn repeated_config_keeps_the_changes_being_made() {
        adw::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let _guard = context.acquire().unwrap();

        let config = PowerLimitThrottle {
            min_power: 150.0,
            thresholds: [("junction".to_owned(), 95)].into(),
            step: 10.0,
            interval_secs: 2,
            hysteresis: 5,
            load_lock: None,
        };

        let dialog = PowerThrottleDialog::detach_default();
        dialog.emit(PowerThrottleDialogMsg::Config(Some(config.clone())));
        while context.pending() {
            context.iteration(false);
        }
        assert!(!dialog.model().load_lock_enabled);

        dialog.widgets().load_lock_switch.set_active(true);
        while context.pending() {
            context.iteration(false);
        }
        assert!(dialog.model().load_lock_enabled);

        dialog.emit(PowerThrottleDialogMsg::Config(Some(config.clone())));
        while context.pending() {
            context.iteration(false);
        }
        assert!(dialog.model().load_lock_enabled);
        assert!(dialog.widgets().load_lock_switch.is_active());

        dialog.emit(PowerThrottleDialogMsg::Config(Some(PowerLimitThrottle {
            min_power: 200.0,
            ..config.clone()
        })));
        while context.pending() {
            context.iteration(false);
        }
        assert!(!dialog.model().load_lock_enabled);
        assert!(!dialog.widgets().load_lock_switch.is_active());
        assert_eq!(
            dialog.model().build_settings().map(|s| s.min_power),
            Some(200.0)
        );
    }
}

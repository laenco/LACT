use super::power_throttle_dialog::power_throttle_status_line;
use crate::{
    APP_BROKER, I18N,
    app::{
        components::{
            adjustment_card::AdjustmentCard,
            adjustment_row::{AdjustmentRow, AdjustmentRowInit, AdjustmentRowMsg},
            page_section::PageSection,
        },
        msg::AppMsg,
        pages::oc_page::{
            OcPageMsg,
            performance_frame::{PerformanceFrame, PerformanceFrameMsg},
        },
        utils::ext::RelmDefaultLauchable,
    },
};
use adw::prelude::*;
use amdgpu_sysfs::gpu_handle::PerformanceLevel;
use i18n_embed_fl::fl;
use lact_schema::{PowerLimitThrottleStatus, PowerStats};
use nvml_wrapper::enums::device::PowerMizerMode;
use relm4::{
    ComponentController, ComponentParts, ComponentSender, WidgetTemplate, css,
    factory::FactoryHashMap,
};

pub struct PowerFrame {
    power: PowerStats,
    power_row: FactoryHashMap<(), AdjustmentRow<()>>,
    performance_frame: relm4::Controller<PerformanceFrame>,
    /// Explains that the power limit is currently lowered by the daemon throttling.
    throttle_note: String,
}

#[derive(Debug)]
pub enum PowerFrameMsg {
    PowerStats(PowerStats),
    /// Values which the daemon is allowed to change while the page is open.
    LiveStats {
        power: PowerStats,
        throttle: Option<PowerLimitThrottleStatus>,
    },
    Performance(PerformanceFrameMsg),
    RefreshVisibility,
    ShowThrottleSettings,
    Reset,
}

#[relm4::component(pub)]
impl relm4::Component for PowerFrame {
    type Init = ();
    type Input = PowerFrameMsg;
    type Output = OcPageMsg;
    type CommandOutput = ();

    view! {
        #[root]
        PageSection {
            set_name: fl!(I18N, "power-section"),
            set_hide_visible_container: true,
            #[watch]
            set_visible: model.is_available(),

            append_header = &gtk::Button {
                set_label: &fl!(I18N, "default-button"),
                connect_clicked => PowerFrameMsg::Reset,

                set_halign: gtk::Align::End,
                set_hexpand: true,
                #[watch]
                set_visible: !model.power_row.is_empty(),
            },
            #[template]
            #[local]
            append_child = &card -> AdjustmentCard {},

            append_child = &gtk::Label {
                set_xalign: 0.0,
                set_wrap: true,
                add_css_class: css::CAPTION,
                #[watch]
                set_label: &model.throttle_note,
                #[watch]
                set_visible: !model.throttle_note.is_empty(),
            },

            append_child = &gtk::Button {
                set_label: &fl!(I18N, "power-throttle-settings-button"),
                set_halign: gtk::Align::Start,
                connect_clicked => PowerFrameMsg::ShowThrottleSettings,
                #[watch]
                set_visible: !model.power_row.is_empty(),
            },
        },
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let card = AdjustmentCard::init(());
        let model = Self {
            power: PowerStats::default(),
            power_row: FactoryHashMap::builder()
                .launch(card.content.clone())
                .forward(APP_BROKER.sender(), |()| AppMsg::SettingsChanged),
            performance_frame: PerformanceFrame::launch_default()
                .forward(sender.output_sender(), |msg| msg),
            throttle_note: String::new(),
        };
        for row in model.performance_frame.widget().rows() {
            card.content.append(row);
            let visibility_sender = sender.clone();
            row.connect_visible_notify(move |_| {
                visibility_sender.input(PowerFrameMsg::RefreshVisibility);
            });
        }

        let widgets = view_output!();
        let rows = model.performance_frame.widget().rows().map(Clone::clone);
        // FIXME: performance rows should be factory items like AdjustmentRow; until then
        // sort to keep the power limit before them when the factory recreates it.
        model.power_row.widget().set_sort_func(move |left, right| {
            rows.iter()
                .position(|row| row == left)
                .cmp(&rows.iter().position(|row| row == right))
                .into()
        });

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            PowerFrameMsg::PowerStats(power) => {
                self.power_row.clear();

                if let Some(value) = power.cap_current {
                    self.power_row.insert(
                        (),
                        AdjustmentRowInit {
                            title: fl!(I18N, "power-cap"),
                            unit: fl!(I18N, "watt"),
                            value,
                            lower: power.cap_min.unwrap_or_default(),
                            upper: power.cap_max.unwrap_or_default(),
                            ..Default::default()
                        },
                    );
                }

                self.power = power;
            }
            PowerFrameMsg::LiveStats { power, throttle } => {
                self.throttle_note = throttle_note(throttle.as_ref());
                self.refresh_power_cap(power.cap_current);
                self.power = power;
            }
            PowerFrameMsg::ShowThrottleSettings => {
                let _ = sender.output(OcPageMsg::ShowPowerThrottleSettings);
            }
            PowerFrameMsg::Performance(msg) => {
                self.performance_frame.emit(msg);
            }
            PowerFrameMsg::RefreshVisibility => (),
            PowerFrameMsg::Reset => {
                if !self.power_row.is_empty() {
                    self.power_row.send(
                        &(),
                        AdjustmentRowMsg::SetValue(self.power.cap_default.unwrap_or_default()),
                    );
                }
            }
        }

        self.update_view(widgets, sender);
    }
}

impl PowerFrame {
    fn is_available(&self) -> bool {
        !self.power_row.is_empty()
            || self
                .performance_frame
                .widget()
                .rows()
                .iter()
                .any(|row| row.get_visible())
    }

    /// Keeps the power limit in sync with the value which is in effect, as the daemon
    /// is allowed to change it while the page is open.
    fn refresh_power_cap(&self, cap: Option<f64>) {
        let Some(cap) = cap else {
            return;
        };
        let Some(row) = self.power_row.get(&()) else {
            return;
        };

        // A value being edited by the user must not be taken away from them.
        if row.get_changed_value().is_some() || (row.get_value() - cap).abs() < f64::EPSILON {
            return;
        }

        self.power_row
            .send(&(), AdjustmentRowMsg::RefreshValue(cap));
    }

    pub fn get_user_cap(&self) -> Option<f64> {
        self.power_row
            .get(&())?
            .get_changed_value()
            .filter(|value| *value != 0.0)
    }

    pub fn performance_level(&self) -> Option<PerformanceLevel> {
        self.performance_frame.model().performance_level()
    }

    pub fn active_power_mizer_mode(&self) -> Option<PowerMizerMode> {
        self.performance_frame.model().active_power_mizer_mode()
    }

    pub fn power_profile_mode(&self) -> Option<u16> {
        self.performance_frame.model().power_profile_mode()
    }

    pub fn power_profile_mode_custom_heuristics(&self) -> Vec<Vec<Option<i32>>> {
        self.performance_frame
            .model()
            .power_profile_mode_custom_heuristics()
    }
}

/// Explains that the power limit is currently lower than the configured one.
fn throttle_note(status: Option<&PowerLimitThrottleStatus>) -> String {
    let Some(status) = status else {
        return String::new();
    };
    if status.effective_cap + 0.001 >= status.base_cap {
        return String::new();
    }

    let mut lines = vec![power_throttle_status_line(status)];

    if !status.breached.is_empty() {
        lines.push(fl!(
            I18N,
            "power-throttle-status-over-threshold",
            sensors = status.breached.join(", "),
        ));
    }

    lines.join("\n")
}

#[cfg(all(test, feature = "gtk-tests"))]
mod tests {
    use super::*;

    fn power_stats(cap: f64) -> PowerStats {
        PowerStats {
            cap_current: Some(cap),
            cap_min: Some(100.0),
            cap_max: Some(350.0),
            ..Default::default()
        }
    }

    #[test]
    #[ignore = "requires a GTK display; run explicitly with --ignored"]
    fn follows_a_power_limit_changed_by_the_daemon() {
        adw::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let _guard = context.acquire().unwrap();

        let frame = PowerFrame::detach_default();
        frame.emit(PowerFrameMsg::PowerStats(power_stats(300.0)));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(frame.model().get_user_cap(), None);

        frame.emit(PowerFrameMsg::LiveStats {
            power: power_stats(261.0),
            throttle: Some(PowerLimitThrottleStatus {
                base_cap: 300.0,
                effective_cap: 261.0,
                load_locked: false,
                breached: vec!["junction".to_owned()],
            }),
        });
        while context.pending() {
            context.iteration(false);
        }

        let model = frame.model();
        let row = model.power_row.get(&()).expect("has a power row");
        assert_eq!(row.get_value(), 261.0);
        // The value set by the daemon must not be treated as an edit of the user.
        assert_eq!(model.get_user_cap(), None);
        assert!(model.throttle_note.contains("junction"));
    }

    #[test]
    #[ignore = "requires a GTK display; run explicitly with --ignored"]
    fn keeps_a_power_limit_being_edited_by_the_user() {
        adw::init().unwrap();
        let context = gtk::glib::MainContext::default();
        let _guard = context.acquire().unwrap();

        let frame = PowerFrame::detach_default();
        frame.emit(PowerFrameMsg::PowerStats(power_stats(300.0)));
        while context.pending() {
            context.iteration(false);
        }

        frame
            .model()
            .power_row
            .send(&(), AdjustmentRowMsg::SetValue(275.0));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(frame.model().get_user_cap(), Some(275.0));

        frame.emit(PowerFrameMsg::LiveStats {
            power: power_stats(261.0),
            throttle: None,
        });
        while context.pending() {
            context.iteration(false);
        }

        assert_eq!(frame.model().get_user_cap(), Some(275.0));
    }
}

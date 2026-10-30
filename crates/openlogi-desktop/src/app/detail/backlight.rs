//! The Device tab's backlight card: a keyboard's HID++ `0x1982` backlight.
//!
//! Nothing is stored on this computer — the keyboard keeps its backlight in
//! its own memory and changes it by itself — so every row shows the
//! keyboard's last reading, and only the controls that reading says it
//! accepts are offered. Each one goes through [`AppState::commit_backlight`],
//! which shows the change at once and lets the keyboard's read-back replace
//! it.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Disableable as _, Icon, IconName, Selectable as _, h_flex, v_flex};
use openlogi_core::hid::{
    BacklightChange, BacklightControls, BacklightDuration, BacklightState, BacklightStatus,
};

use crate::state::{AppState, BacklightLoad, DeviceKey, DurationSteps};
use crate::ui::components::{PanelCard, Pill, Toggle, toggle_row};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the card needs about the selected device.
pub(super) struct BacklightFacts {
    key: DeviceKey,
    load: BacklightLoad,
    reachable: bool,
    /// Which timer steps are on screen. Not a keyboard setting.
    steps: DurationSteps,
}

impl BacklightFacts {
    /// The selected device's backlight facts, or `None` when nothing is
    /// selected or the device reports no backlight control.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_backlight_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_backlight_load(),
            reachable: record.route.is_some(),
            steps: state.duration_steps(),
        })
    }
}

/// The card, or `None` to hide it: no control, a keyboard that turned out not
/// to have one, or an offline keyboard (the details card already says it is
/// offline, and there is no reading to show).
pub(super) fn backlight_card(facts: Option<BacklightFacts>, pal: Palette) -> Option<AnyElement> {
    let BacklightFacts {
        key,
        load,
        reachable,
        steps,
    } = facts?;
    let body: AnyElement = match load {
        BacklightLoad::Ready(state) => backlight_rows(*state, steps, pal).into_any_element(),
        BacklightLoad::Unknown | BacklightLoad::Loading if !reachable => return None,
        BacklightLoad::Unknown | BacklightLoad::Loading => {
            status_line(tr!("device.reading_backlight"), pal).into_any_element()
        }
        BacklightLoad::Failed(_) => retry_line(
            "backlight-retry",
            tr!("device.couldnt_read_backlight_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_backlight_read(&key)),
        )
        .into_any_element(),
        BacklightLoad::Unsupported(_) => return None,
    };
    Some(PanelCard::new(tr!("device.backlight"), Icon::new(IconName::Sun), body).into_any_element())
}

/// One row per control the keyboard accepts. While the backlight is off, the
/// level and how it is chosen have nothing to act on, so those rows are shown
/// but disabled; battery saving still applies.
fn backlight_rows(state: BacklightState, steps: DurationSteps, pal: Palette) -> impl IntoElement {
    let lit_row = toggle_row(
        "backlight-row",
        pal,
        tr!("device.light_the_keys"),
        lit_caption(state),
        Toggle::new("backlight-toggle")
            .selected(state.enabled)
            .on_change(|on, _window, cx| commit(cx, BacklightChange::Enabled(*on))),
    );
    v_flex()
        .gap_4()
        .child(lit_row)
        .when(
            state.controls.contains(BacklightControls::AUTOMATIC),
            |rows| {
                rows.child(toggle_row(
                    "automatic-brightness-row",
                    pal,
                    tr!("device.automatic_brightness"),
                    tr!("device.automatic_brightness_description"),
                    Toggle::new("automatic-brightness-toggle")
                        .selected(state.is_automatic())
                        .disabled(!state.enabled)
                        .on_change(|on, _window, cx| commit(cx, BacklightChange::Automatic(*on))),
                ))
            },
        )
        .when(state.controls.contains(BacklightControls::LEVEL), |rows| {
            rows.child(level_row(state, pal))
        })
        .when(
            state.controls.contains(BacklightControls::POWER_SAVE),
            |rows| {
                rows.child(toggle_row(
                    "battery-saving-row",
                    pal,
                    tr!("device.battery_saving"),
                    tr!("device.battery_saving_description"),
                    Toggle::new("battery-saving-toggle")
                        .selected(state.power_save)
                        .on_change(|on, _window, cx| commit(cx, BacklightChange::PowerSave(*on))),
                ))
            },
        )
        .when(
            state.controls.contains(BacklightControls::DURATION),
            |rows| rows.child(duration_row(state, steps, pal)),
        )
}

/// Why the keys are dark when the firmware, not the user, turned them off;
/// otherwise where the setting lives.
fn lit_caption(state: BacklightState) -> gpui::SharedString {
    match state.status {
        _ if !state.enabled => tr!("device.light_the_keys_description"),
        BacklightStatus::DisabledByCriticalBattery => tr!("device.backlight_off_battery"),
        BacklightStatus::AlsSaturated => tr!("device.backlight_off_bright"),
        BacklightStatus::DisabledBySoftware
        | BacklightStatus::AlsAutomatic
        | BacklightStatus::TemporaryManual
        | BacklightStatus::PermanentManual => tr!("device.light_the_keys_description"),
    }
}

/// The brightness levels as pills, `0` (dark) up to the highest the keyboard
/// reports; the selected pill is the level the keys show now, whoever chose
/// it.
fn level_row(state: BacklightState, pal: Palette) -> impl IntoElement {
    v_flex()
        .debug_selector(|| "backlight-level-row".into())
        .gap_2()
        .child(
            div()
                .text_body()
                .text_color(pal.text_primary)
                .child(tr!("device.backlight_level")),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .children((0..state.nb_levels).map(|level| {
                    Pill::new(("backlight-level", usize::from(level)), level.to_string())
                        .selected(level == state.current_level)
                        .disabled(!state.enabled)
                        .on_click(move |_event, _window, cx| {
                            commit(cx, BacklightChange::Level(level));
                        })
                })),
        )
        .when(
            state.controls.contains(BacklightControls::AUTOMATIC),
            |row| {
                row.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("device.backlight_level_leaves_automatic")),
                )
            },
        )
}

/// The timer, 5 through 30 seconds plus the longer steps when extended
/// durations are on, through the two hours `0x1982` can store. The selected
/// pill is the keyboard's current timer when it is one of the steps on
/// screen.
fn duration_row(state: BacklightState, steps: DurationSteps, pal: Palette) -> impl IntoElement {
    v_flex()
        .debug_selector(|| "backlight-duration-row".into())
        .gap_2()
        .child(
            div()
                .text_body()
                .text_color(pal.text_primary)
                .child(tr!("device.backlight_duration")),
        )
        .children(
            steps
                .steps()
                .iter()
                .map(|table| duration_pills(state, table)),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(duration_caption(state, steps)),
        )
        .child(toggle_row(
            "extended-durations-row",
            pal,
            tr!("device.enable_extended_durations"),
            tr!("device.extended_durations_warning"),
            Toggle::new("extended-durations-toggle")
                .selected(steps == DurationSteps::Extended)
                .on_change(|on, _window, cx| {
                    AppState::apply(cx, |state| {
                        state.set_duration_steps(if *on {
                            DurationSteps::Extended
                        } else {
                            DurationSteps::Short
                        })
                    });
                }),
        ))
}

fn duration_pills(state: BacklightState, steps: &[u16]) -> impl IntoElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(steps.iter().copied().map(|seconds| {
            #[expect(
                clippy::expect_used,
                reason = "every panel step is covered by \
                          `every_step_the_panel_shows_is_a_valid_timer`; a const \
                          constructor does not exist"
            )]
            let duration = BacklightDuration::try_from(seconds)
                .expect("the panel only offers steps the keyboard can store");
            Pill::new(
                ("backlight-duration", usize::from(seconds)),
                duration_label(seconds),
            )
            .selected(Some(duration) == state.duration)
            .disabled(!state.enabled)
            .on_click(move |_event, _window, cx| {
                commit(cx, BacklightChange::Duration(duration));
            })
        }))
}

/// A step label: seconds under a minute, otherwise minutes or hours. Never
/// built by concatenating a translated unit onto a number the other way
/// around — each shape is its own catalog string.
fn duration_label(seconds: u16) -> gpui::SharedString {
    if seconds < 60 {
        tr!("device.backlight_duration_seconds", seconds => seconds)
    } else if seconds.is_multiple_of(3_600) {
        tr!(
            "device.backlight_duration_hours",
            hours => seconds / 3_600
        )
    } else {
        tr!(
            "device.backlight_duration_minutes",
            minutes => seconds / 60
        )
    }
}

/// The usual explanation, or the seconds the keyboard actually stored when
/// they are not one of the steps this row is showing.
fn duration_caption(state: BacklightState, steps: DurationSteps) -> gpui::SharedString {
    match state.duration {
        Some(timer) if !steps.contains(timer.seconds()) => tr!(
            "device.backlight_duration_other",
            seconds => timer.seconds()
        ),
        _ => tr!("device.backlight_duration_description"),
    }
}

fn commit(cx: &mut App, change: BacklightChange) {
    AppState::apply(cx, |state| state.commit_backlight(change));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{
        Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div,
        px, size,
    };
    use openlogi_core::hid::{
        BacklightControls, BacklightDuration, BacklightMode, BacklightState, BacklightStatus,
    };

    /// A test duration; the type guarantees every constructed value is legal.
    fn duration(seconds: u16) -> BacklightDuration {
        BacklightDuration::try_from(seconds).expect("a listed step is a valid duration")
    }

    use super::{BacklightFacts, backlight_card, duration_caption, lit_caption};
    use crate::services::i18n::LOCALE_LOCK;
    use crate::state::{DeviceKey, DurationSteps, Load};
    use crate::ui::theme;

    const ROWS: [&str; 6] = [
        "backlight-row",
        "automatic-brightness-row",
        "backlight-level-row",
        "battery-saving-row",
        "backlight-duration-row",
        "extended-durations-row",
    ];

    fn reading(controls: BacklightControls) -> BacklightState {
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 2,
            nb_levels: 5,
            power_save: true,
            duration: Some(duration(30)),
            controls,
        }
    }

    /// Hosts the card at a narrow card width so long captions have to wrap.
    struct CardHost(BacklightState);

    impl Render for CardHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let facts = BacklightFacts {
                key: DeviceKey::from("direct:046d:b35b:unit:a393cae0"),
                load: Load::Ready(Arc::new(self.0)),
                reachable: true,
                steps: match self.0.duration {
                    Some(timer) if timer.seconds() > 30 => DurationSteps::Extended,
                    _ => DurationSteps::Short,
                },
            };
            div()
                .w(px(360.))
                .children(backlight_card(Some(facts), theme::palette(cx)))
        }
    }

    fn draw(cx: &mut TestAppContext, state: BacklightState) -> &mut gpui::VisualTestContext {
        cx.update(gpui_component::init);
        let (_view, cx) = cx.add_window_view(|_, _| CardHost(state));
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx
    }

    #[gpui::test]
    fn every_offered_control_gets_a_row_that_keeps_its_toggle_inside(cx: &mut TestAppContext) {
        // Finnish carries the longest backlight captions.
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("fi");
        let cx = draw(
            cx,
            reading(
                BacklightControls::AUTOMATIC
                    | BacklightControls::LEVEL
                    | BacklightControls::POWER_SAVE
                    | BacklightControls::DURATION,
            ),
        );

        for row in ROWS {
            let bounds = cx
                .debug_bounds(row)
                .unwrap_or_else(|| panic!("{row} renders"));
            assert!(
                bounds.size.width <= px(360.),
                "{row} {bounds:?} must fit its card"
            );
        }
        for (row, control) in [
            ("backlight-row", "backlight-row-control"),
            (
                "automatic-brightness-row",
                "automatic-brightness-row-control",
            ),
            ("battery-saving-row", "battery-saving-row-control"),
            ("extended-durations-row", "extended-durations-row-control"),
        ] {
            let bounds = cx.debug_bounds(row).expect("row");
            let control = cx
                .debug_bounds(control)
                .unwrap_or_else(|| panic!("{row} renders its toggle"));
            assert!(
                control.left() >= bounds.left() && control.right() <= bounds.right(),
                "{row}'s toggle {control:?} must sit inside its row {bounds:?}"
            );
        }
    }

    #[gpui::test]
    fn a_keyboard_offering_only_on_off_gets_only_that_row(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let cx = draw(cx, reading(BacklightControls::default()));

        assert!(cx.debug_bounds("backlight-row").is_some());
        for row in &ROWS[1..] {
            assert!(cx.debug_bounds(row).is_none(), "{row} is not offered");
        }
    }

    #[test]
    fn the_caption_says_why_the_firmware_turned_the_keys_off() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let lit = reading(BacklightControls::default());
        let with = |status| BacklightState { status, ..lit };
        assert_eq!(
            lit_caption(with(BacklightStatus::DisabledByCriticalBattery)),
            tr!("device.backlight_off_battery")
        );
        assert_eq!(
            lit_caption(with(BacklightStatus::AlsSaturated)),
            tr!("device.backlight_off_bright")
        );
        assert_eq!(lit_caption(lit), tr!("device.light_the_keys_description"));
        // Turned off by the user: no firmware reason to give.
        let off = BacklightState {
            enabled: false,
            ..with(BacklightStatus::DisabledByCriticalBattery)
        };
        assert_eq!(lit_caption(off), tr!("device.light_the_keys_description"));
    }

    #[test]
    fn an_unoffered_duration_is_named_instead_of_being_hidden() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let offered = reading(BacklightControls::DURATION);
        assert_eq!(
            duration_caption(offered, DurationSteps::Short),
            tr!("device.backlight_duration_description")
        );
        let longer = BacklightState {
            duration: Some(duration(55)),
            ..offered
        };
        assert_eq!(
            duration_caption(longer, DurationSteps::Extended),
            tr!("device.backlight_duration_other", seconds => 55u16)
        );
        // A two-hour timer is one of the extended steps, so it needs no aside.
        let max = BacklightState {
            duration: Some(duration(7_200)),
            ..offered
        };
        assert_eq!(
            duration_caption(max, DurationSteps::Extended),
            tr!("device.backlight_duration_description")
        );
    }
}

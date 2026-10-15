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
use gpui_component::{
    Disableable as _, Icon, IconName, Selectable as _, button::Button, h_flex, v_flex,
};
use openlogi_core::hid::{BacklightChange, BacklightState, BacklightStatus};

use crate::state::{AppState, BacklightLoad, DeviceKey};
use crate::ui::components::{PanelCard, Toggle, toggle_row};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the card needs about the selected device.
pub(super) struct BacklightFacts {
    key: DeviceKey,
    load: BacklightLoad,
    reachable: bool,
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
    } = facts?;
    let body: AnyElement = match load {
        BacklightLoad::Ready(state) => backlight_rows(*state, pal).into_any_element(),
        BacklightLoad::Unknown | BacklightLoad::Loading if !reachable => return None,
        BacklightLoad::Unknown | BacklightLoad::Loading => {
            status_line(tr!("device.reading_backlight"), pal).into_any_element()
        }
        BacklightLoad::Failed(_) => retry_line(
            "backlight-retry",
            tr!("device.couldnt_read_backlight_click_to_retry"),
            pal,
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
fn backlight_rows(state: BacklightState, pal: Palette) -> impl IntoElement {
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
        .when(state.controls.automatic, |rows| {
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
        })
        .when(state.controls.level, |rows| {
            rows.child(level_row(state, pal))
        })
        .when(state.controls.power_save, |rows| {
            rows.child(toggle_row(
                "battery-saving-row",
                pal,
                tr!("device.battery_saving"),
                tr!("device.battery_saving_description"),
                Toggle::new("battery-saving-toggle")
                    .selected(state.power_save)
                    .on_change(|on, _window, cx| commit(cx, BacklightChange::PowerSave(*on))),
            ))
        })
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
                    Button::new(("backlight-level", usize::from(level)))
                        .compact()
                        .label(level.to_string())
                        .selected(level == state.current_level)
                        .disabled(!state.enabled)
                        .on_click(move |_event, _window, cx| {
                            commit(cx, BacklightChange::Level(level));
                        })
                })),
        )
        .when(state.controls.automatic, |row| {
            row.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("device.backlight_level_leaves_automatic")),
            )
        })
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
    use openlogi_core::hid::{BacklightControls, BacklightMode, BacklightState, BacklightStatus};

    use super::{BacklightFacts, backlight_card, lit_caption};
    use crate::services::i18n::LOCALE_LOCK;
    use crate::state::{DeviceKey, Load};
    use crate::ui::theme;

    const ROWS: [&str; 4] = [
        "backlight-row",
        "automatic-brightness-row",
        "backlight-level-row",
        "battery-saving-row",
    ];

    fn reading(controls: BacklightControls) -> BacklightState {
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 2,
            nb_levels: 5,
            power_save: true,
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
            reading(BacklightControls {
                automatic: true,
                level: true,
                power_save: true,
            }),
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
}

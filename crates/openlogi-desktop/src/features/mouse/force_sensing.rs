//! The press-force row of the Haptic Sense Panel's inspector.
//!
//! A device whose force-sensing button (HID++ `0x19c0`) lets software change
//! its force, such as the MX Master 4, gets a slider from "Light" to "Firm"
//! over the range the device reports. The force has no unit a user could
//! read, so the slider shows no number. A release goes through
//! [`AppState::commit_force_sensing`], which writes the device; the newest
//! write's read-back moves the slider and is saved when it is the force asked
//! for. When the device refuses the write, a note under the slider says so.
//! While the reading is on its way, the row says so in place of the slider,
//! as the pointer-speed row does.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
};
use gpui_component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::Slider,
    v_flex,
};
use openlogi_core::binding::ButtonId;
use openlogi_core::hid::ForceSensingInfo;

use super::hotspots::MouseControlId;
use crate::state::{AppState, DeviceKey, ForceSensingLoad};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the row needs about the selected device.
pub(super) struct ForceSensingFacts {
    key: DeviceKey,
    load: ForceSensingLoad,
    /// The reading to put the slider on, or `None` while the slider must
    /// not show.
    reading: Option<ForceSensingInfo>,
    /// The device refused the last write.
    refused: bool,
    /// The force differs from the device's default, so the reset button
    /// has something to do.
    resettable: bool,
}

impl ForceSensingFacts {
    /// The selected device's facts, or `None` when the row must not show:
    /// `selected` is not the haptic panel, an app profile is being edited
    /// (the force is device-wide, as the wheel-hold switch is), no device is
    /// selected, the device has no force-sensing button whose force its
    /// probe found changeable, it has no route to write through, or its
    /// reading says the force may not change.
    pub(super) fn current(selected: Option<MouseControlId>, cx: &App) -> Option<Self> {
        if selected != Some(MouseControlId::Button(ButtonId::HapticPanel)) {
            return None;
        }
        let state = AppState::try_read(cx)?;
        if state.editing_app().is_some() || !state.current_force_sensing_supported() {
            return None;
        }
        let record = state.current_record()?;
        record.route.as_ref()?;
        let load = state.current_force_sensing_load();
        let reading = state.current_force_sensing_reading();
        row_shows(reading.as_ref(), &load).then(|| Self {
            key: record.device_key(),
            load,
            reading,
            refused: state.current_force_sensing_refused(),
            resettable: state.current_force_sensing_reset().is_some(),
        })
    }
}

/// Whether the row shows for a device whose probe found its force
/// changeable: with the changeable `reading`
/// [`AppState::current_force_sensing_reading`] gave, or while `load` is on
/// its way, including a retry after a failure, so the row keeps its place.
/// Only a reading that says otherwise hides it.
fn row_shows(reading: Option<&ForceSensingInfo>, load: &ForceSensingLoad) -> bool {
    reading.is_some()
        || match load {
            ForceSensingLoad::Unknown | ForceSensingLoad::Loading | ForceSensingLoad::Failed(_) => {
                true
            }
            ForceSensingLoad::Ready(_) | ForceSensingLoad::Unsupported(_) => false,
        }
}

/// The slider, kept across frames, with the device and range it was built
/// for.
pub(super) struct ForceSensingSlider {
    key: DeviceKey,
    /// The device-reported `(min, max)`. A slider's range is fixed when it
    /// is built, so a new range builds a new slider.
    bounds: (u16, u16),
    slider: CommitSlider<u16>,
}

impl ForceSensingSlider {
    /// Keep `current` in step with `facts`: drop the slider when it must not
    /// show, build one for a newly selected device or range, or move the
    /// thumb when the force changed underneath it.
    pub(super) fn sync<V: 'static>(
        current: &mut Option<Self>,
        facts: Option<&ForceSensingFacts>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let Some((key, info)) = facts.and_then(|facts| Some((&facts.key, facts.reading?))) else {
            *current = None;
            return;
        };
        let force = info.current;
        let bounds = (info.min, info.max);
        if let Some(existing) = current
            .as_ref()
            .filter(|existing| existing.key == *key && existing.bounds == bounds)
        {
            existing.slider.sync(force, window, cx);
            return;
        }
        let slider = CommitSlider::new(
            SliderRange::new(info.min, info.max),
            force,
            cx,
            // `commit_force_sensing` resolves the device when the thumb is
            // released, so a device switch mid-drag writes the device now on
            // screen.
            |_, force, cx| AppState::apply(cx, |state| state.commit_force_sensing(force)),
        );
        *current = Some(Self {
            key: key.clone(),
            bounds,
            slider,
        });
    }
}

/// The row as the inspector receives it: the facts and the slider kept for
/// them.
#[derive(Clone, Copy)]
pub(super) struct PressForce<'a> {
    facts: &'a ForceSensingFacts,
    slider: Option<&'a ForceSensingSlider>,
}

impl<'a> PressForce<'a> {
    /// The row for `facts`, or `None` when it must not show.
    pub(super) fn of(
        facts: Option<&'a ForceSensingFacts>,
        slider: Option<&'a ForceSensingSlider>,
    ) -> Option<Self> {
        facts.map(|facts| Self { facts, slider })
    }
}

/// The icon button at the slider's end that puts the force back to the
/// device's default, with the inspector's revert icon. Disabled while the
/// force already sits there.
fn reset_button(resettable: bool) -> impl IntoElement {
    let label = tr!("pointer.press_force_reset");
    div().debug_selector(|| "force-sensing-reset".into()).child(
        Button::new("force-sensing-reset")
            .ghost()
            .small()
            .icon(IconName::Undo)
            .accessibility_label(label.clone())
            .tooltip(label)
            .disabled(!resettable)
            .on_click(|_, _, cx| AppState::apply(cx, AppState::reset_force_sensing)),
    )
}

/// The row: the title and what the setting does, then the slider between
/// its "Light" and "Firm" ends, a status line while the reading is on its
/// way, or a retry line when the read failed. A hairline above it parts the
/// device-wide setting from the button's bindings.
pub(super) fn force_sensing_row(press_force: PressForce<'_>, pal: Palette) -> AnyElement {
    let PressForce { facts, slider } = press_force;
    let key = facts.key.clone();
    let body = match (slider, &facts.load) {
        (Some(slider), _) => h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.press_force_light")),
            )
            .child(
                div()
                    .flex_1()
                    .child(Slider::new(slider.slider.slider()).horizontal()),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.press_force_firm")),
            )
            .child(reset_button(facts.resettable))
            .into_any_element(),
        (None, ForceSensingLoad::Failed(_)) => retry_line(
            "force-sensing-retry",
            tr!("pointer.couldnt_read_press_force_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_force_sensing_read(&key)),
        )
        .into_any_element(),
        // A retry clears the failure, so the row keeps its place and says
        // the device is being read.
        (
            None,
            ForceSensingLoad::Unknown
            | ForceSensingLoad::Loading
            | ForceSensingLoad::Ready(_)
            | ForceSensingLoad::Unsupported(_),
        ) => status_line(tr!("pointer.reading_press_force"), pal).into_any_element(),
    };
    v_flex()
        .debug_selector(|| "force-sensing-row".into())
        .gap_2()
        .w_full()
        .pt_3()
        .border_t_1()
        .border_color(pal.border)
        .child(
            v_flex()
                .child(div().text_body().child(tr!("pointer.press_force")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("pointer.press_force_description")),
                ),
        )
        .child(body)
        .children(facts.refused.then(|| {
            div()
                .debug_selector(|| "force-sensing-refusal".into())
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("pointer.press_force_not_applied"))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
    use gpui_component::slider::{SliderEvent, SliderState};
    use openlogi_core::binding::ButtonId;
    use openlogi_core::config::Config;
    use openlogi_core::hid::ForceSensingInfo;
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::hotspots::MouseControlId;
    use super::super::view::MouseModelView;
    use crate::services::i18n::LOCALE_LOCK;
    use crate::services::ipc::{Command, SetForceSensing};
    use crate::state::tests::force_sensing::{PANEL, answer_force_read, install, mouse_state};
    use crate::state::{AppState, DeviceRecord};

    /// The mouse view over a mouse with a force-sensing button whose read
    /// answered `reading`, with the haptic panel selected.
    fn panel(
        reading: ForceSensingInfo,
        cx: &mut TestAppContext,
    ) -> (
        Entity<MouseModelView>,
        &mut VisualTestContext,
        UnboundedReceiver<Command>,
    ) {
        cx.update(gpui_component::init);
        let (state, mut receiver) = mouse_state(true, Config::ephemeral());
        install(state, cx);
        answer_force_read(&mut receiver, reading);
        cx.run_until_parked();
        let (panel, cx) = cx.add_window_view(MouseModelView::new);
        cx.simulate_resize(size(px(1200.), px(1000.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        select(&panel, ButtonId::HapticPanel, cx);
        (panel, cx, receiver)
    }

    fn select(panel: &Entity<MouseModelView>, button: ButtonId, cx: &mut VisualTestContext) {
        panel.update(cx, |panel, _| {
            panel.select(MouseControlId::Button(button));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn slider(
        panel: &Entity<MouseModelView>,
        cx: &VisualTestContext,
    ) -> Option<Entity<SliderState>> {
        panel.read_with(cx, |panel, _| {
            panel
                .force_sensing
                .as_ref()
                .map(|row| row.slider.slider().clone())
        })
    }

    #[gpui::test]
    fn the_slider_rests_on_the_devices_force_and_a_release_writes_it(cx: &mut TestAppContext) {
        let (panel, cx, mut receiver) = panel(PANEL, cx);
        let slider = slider(&panel, cx).expect("a changeable force shows the slider");
        panel.read_with(cx, |panel, cx| {
            let row = panel.force_sensing.as_ref().expect("the row");
            assert_eq!(row.slider.value(cx), PANEL.current);
        });
        slider.read_with(cx, |state, _| {
            assert_eq!(
                (state.min_value(), state.max_value()),
                (f32::from(PANEL.min), f32::from(PANEL.max)),
                "the range is the one the device reported"
            );
        });

        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(6000.0.into())));
        cx.run_until_parked();

        assert_eq!(force_writes(&mut receiver), [6000]);
    }

    #[gpui::test]
    fn the_row_is_hidden_while_the_device_does_not_let_the_force_change(cx: &mut TestAppContext) {
        let (panel, cx, _receiver) = panel(
            ForceSensingInfo {
                changeable: false,
                ..PANEL
            },
            cx,
        );

        assert!(slider(&panel, cx).is_none());
    }

    #[gpui::test]
    fn a_device_reading_that_moves_reseats_the_thumb(cx: &mut TestAppContext) {
        let (panel, cx, mut receiver) = panel(PANEL, cx);
        let key = cx.update(|_, cx| {
            AppState::try_read(cx)
                .and_then(|state| state.current_record().map(DeviceRecord::device_key))
                .expect("the mouse is selected")
        });
        cx.update(|_, cx| AppState::apply(cx, |state| state.commit_force_sensing(6000)));
        let write = std::iter::from_fn(|| receiver.try_recv().ok())
            .find_map(|command| match command {
                Command::SetForceSensing(SetForceSensing { write, .. }) => Some(write),
                _ => None,
            })
            .expect("the release writes the device");

        // A newer write at the agent took the turn, so this one read the
        // device instead.
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state.apply_force_sensing_written(
                    &key,
                    write,
                    6000,
                    Ok(ForceSensingInfo {
                        current: 4500,
                        ..PANEL
                    }),
                )
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        panel.read_with(cx, |panel, cx| {
            let row = panel.force_sensing.as_ref().expect("the row");
            assert_eq!(row.slider.value(cx), 4500);
        });
    }

    #[gpui::test]
    fn only_the_haptic_panel_in_the_default_profile_shows_the_row(cx: &mut TestAppContext) {
        let (panel, cx, _receiver) = panel(PANEL, cx);
        assert!(
            cx.debug_bounds("force-sensing-row").is_some(),
            "the haptic panel's inspector shows its press force"
        );

        select(&panel, ButtonId::Back, cx);
        assert!(
            cx.debug_bounds("force-sensing-row").is_none(),
            "another button's inspector does not"
        );
        assert!(slider(&panel, cx).is_none(), "the slider goes with the row");

        select(&panel, ButtonId::HapticPanel, cx);
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state.set_editing_app(Some("com.apple.Safari".into()))
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("force-sensing-row").is_none(),
            "the force is device-wide, so an app profile does not show it"
        );
    }

    fn click_reset(cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds("force-sensing-reset")
            .expect("the slider has a reset button");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
    }

    fn force_writes(receiver: &mut UnboundedReceiver<Command>) -> Vec<u16> {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .filter_map(|command| match command {
                Command::SetForceSensing(SetForceSensing { force, .. }) => Some(force),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn the_reset_button_writes_the_devices_default(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (_panel, cx, mut receiver) = panel(
            ForceSensingInfo {
                current: 6000,
                ..PANEL
            },
            cx,
        );
        let _ = force_writes(&mut receiver);

        click_reset(cx);

        assert_eq!(force_writes(&mut receiver), [PANEL.default]);
    }

    #[gpui::test]
    fn the_reset_button_does_nothing_at_the_default(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (_panel, cx, mut receiver) = panel(PANEL, cx);
        let _ = force_writes(&mut receiver);

        click_reset(cx);

        assert!(
            force_writes(&mut receiver).is_empty(),
            "the button is disabled"
        );
    }

    #[gpui::test]
    fn the_gesture_inspector_shows_the_row_too(cx: &mut TestAppContext) {
        let (panel, cx, _receiver) = panel(PANEL, cx);
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state.commit_gesture_mode(ButtonId::HapticPanel, true)
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.update(|_, cx| {
            let state = AppState::try_read(cx).expect("state is installed");
            assert!(
                state
                    .gesture_bindings()
                    .contains_key(&ButtonId::HapticPanel)
            );
        });
        assert!(cx.debug_bounds("force-sensing-row").is_some());
        assert!(slider(&panel, cx).is_some());
    }

    #[gpui::test]
    fn a_mouse_without_a_force_sensing_button_shows_no_row(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (state, _receiver) = mouse_state(false, Config::ephemeral());
        install(state, cx);
        let (panel, cx) = cx.add_window_view(MouseModelView::new);
        cx.simulate_resize(size(px(1200.), px(1000.)));
        select(&panel, ButtonId::HapticPanel, cx);

        assert!(cx.debug_bounds("force-sensing-row").is_none());
    }

    /// A retry turns a failed read back into a loading one; the row stays,
    /// so the retry line becomes a reading line in place.
    #[test]
    fn only_a_reading_that_rules_the_force_out_hides_the_row() {
        use std::sync::Arc;

        use super::row_shows;
        use crate::state::ForceSensingLoad;

        let unchangeable = ForceSensingInfo {
            changeable: false,
            ..PANEL
        };
        // The reading is the one `current_force_sensing_reading` gives for
        // each load: only a ready, changeable one.
        for (load, reading, shows) in [
            (ForceSensingLoad::Unknown, None, true),
            (ForceSensingLoad::Loading, None, true),
            (
                ForceSensingLoad::Failed("unreachable".to_string()),
                None,
                true,
            ),
            (ForceSensingLoad::Ready(Arc::new(PANEL)), Some(PANEL), true),
            (ForceSensingLoad::Ready(Arc::new(unchangeable)), None, false),
            (
                ForceSensingLoad::Unsupported("no 0x19c0".to_string()),
                None,
                false,
            ),
        ] {
            assert_eq!(row_shows(reading.as_ref(), &load), shows, "{load:?}");
        }
    }
}

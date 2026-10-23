//! The press-force row of the Haptic Sense Panel settings.
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
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::hid::ForceSensingInfo;

use crate::state::{AppState, DeviceKey, ForceSensingLoad};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the row needs about the selected device.
pub(super) struct ForceSensingFacts {
    key: DeviceKey,
    load: ForceSensingLoad,
    /// The device refused the last write.
    refused: bool,
}

impl ForceSensingFacts {
    /// The selected device's facts, or `None` when the row must not show:
    /// nothing is selected, the device has no force-sensing button whose
    /// force its probe found changeable, it has no route to write through,
    /// or its reading says the force may not change.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_force_sensing_supported() {
            return None;
        }
        let record = state.current_record()?;
        record.route.as_ref()?;
        let load = state.current_force_sensing_load();
        row_shows(&load).then(|| Self {
            key: record.device_key(),
            load,
            refused: state.current_force_sensing_refused(),
        })
    }

    /// The reading to put the slider on, or `None` while the slider must
    /// not show.
    fn reading(&self) -> Option<ForceSensingInfo> {
        match &self.load {
            ForceSensingLoad::Ready(info) if info.changeable => Some(**info),
            ForceSensingLoad::Ready(_)
            | ForceSensingLoad::Unknown
            | ForceSensingLoad::Loading
            | ForceSensingLoad::Failed(_)
            | ForceSensingLoad::Unsupported(_) => None,
        }
    }
}

/// Whether the row shows for a device whose probe found its force
/// changeable. Only a reading that says otherwise hides it; a reading on its
/// way, including a retry after a failure, keeps the row in place.
fn row_shows(load: &ForceSensingLoad) -> bool {
    match load {
        ForceSensingLoad::Ready(info) => info.changeable,
        ForceSensingLoad::Unknown | ForceSensingLoad::Loading | ForceSensingLoad::Failed(_) => true,
        ForceSensingLoad::Unsupported(_) => false,
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
        let Some((key, info)) = facts.and_then(|facts| Some((&facts.key, facts.reading()?))) else {
            *current = None;
            return;
        };
        let force = info.clamp(info.current);
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

/// The row: the title and what the setting does, then the slider between
/// its "Light" and "Firm" ends, a status line while the reading is on its
/// way, or a retry line when the read failed.
pub(super) fn force_sensing_row(
    facts: ForceSensingFacts,
    slider: Option<&ForceSensingSlider>,
    pal: Palette,
) -> AnyElement {
    let ForceSensingFacts { key, load, refused } = facts;
    let body = match (slider, load) {
        (Some(slider), _) => h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("action_ring.press_force_light")),
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
                    .child(tr!("action_ring.press_force_firm")),
            )
            .into_any_element(),
        (None, ForceSensingLoad::Failed(_)) => retry_line(
            "force-sensing-retry",
            tr!("action_ring.couldnt_read_press_force_click_to_retry"),
            pal,
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
        ) => status_line(tr!("action_ring.reading_press_force"), pal).into_any_element(),
    };
    v_flex()
        .gap_2()
        .w_full()
        .child(
            v_flex()
                .child(div().text_body().child(tr!("action_ring.press_force")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("action_ring.press_force_description")),
                ),
        )
        .child(body)
        .children(refused.then(|| {
            div()
                .debug_selector(|| "force-sensing-refusal".into())
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("action_ring.press_force_not_applied"))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use gpui_component::slider::{SliderEvent, SliderState};
    use openlogi_core::config::Config;
    use openlogi_core::hid::ForceSensingInfo;
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::ActionRingPanel;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::{Command, SetForceSensing};
    use crate::state::tests::force_sensing::{PANEL, answer_force_read, install, mouse_inventory};
    use crate::state::{AppState, DeviceRecord, Sources};

    /// The Actions Ring panel over a mouse with a force-sensing button whose
    /// read answered `reading`.
    fn panel(
        reading: ForceSensingInfo,
        cx: &mut TestAppContext,
    ) -> (
        Entity<ActionRingPanel>,
        &mut VisualTestContext,
        UnboundedReceiver<Command>,
    ) {
        cx.update(gpui_component::init);
        let resolver = AssetResolver::new();
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::new(Sources {
            inventories: &[mouse_inventory(true)],
            ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
        });
        install(state, cx);
        answer_force_read(&mut receiver, reading);
        cx.run_until_parked();
        let (panel, cx) = cx.add_window_view(|_, cx| ActionRingPanel::new(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (panel, cx, receiver)
    }

    fn slider(
        panel: &Entity<ActionRingPanel>,
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

        let writes: Vec<u16> = std::iter::from_fn(|| receiver.try_recv().ok())
            .filter_map(|command| match command {
                Command::SetForceSensing(SetForceSensing { force, .. }) => Some(force),
                _ => None,
            })
            .collect();
        assert_eq!(writes, [6000]);
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
        for (load, shows) in [
            (ForceSensingLoad::Unknown, true),
            (ForceSensingLoad::Loading, true),
            (ForceSensingLoad::Failed("unreachable".to_string()), true),
            (ForceSensingLoad::Ready(Arc::new(PANEL)), true),
            (ForceSensingLoad::Ready(Arc::new(unchangeable)), false),
            (
                ForceSensingLoad::Unsupported("no 0x19c0".to_string()),
                false,
            ),
        ] {
            assert_eq!(row_shows(&load), shows, "{load:?}");
        }
    }
}

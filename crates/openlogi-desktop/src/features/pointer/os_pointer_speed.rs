//! The OS-level pointer-speed override for the selected mouse.

use gpui::{AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, Window};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{Selectable as _, v_flex};

use crate::state::{AppState, DeviceKey};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::Palette;

const MIN_SPEED: i32 = -100;
const MAX_SPEED: i32 = 100;
const DEFAULT_SPEED: i32 = 0;

/// The selected device and its persisted compositor speed override.
pub(super) struct OsPointerSpeedFacts {
    key: DeviceKey,
    speed: i32,
    configured: bool,
    accel_enabled: Option<bool>,
}

impl OsPointerSpeedFacts {
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        let record = state.current_record()?;
        let configured_speed = state.current_os_pointer_speed();
        let accel_enabled = state.current_os_pointer_accel();
        Some(Self {
            key: record.device_key(),
            speed: configured_speed.map_or(DEFAULT_SPEED, speed_to_slider),
            configured: configured_speed.is_some(),
            accel_enabled,
        })
    }
}

/// Retained slider state, rebuilt when the active device changes.
pub(super) struct OsPointerSpeedSlider {
    key: DeviceKey,
    slider: CommitSlider<i32>,
}

impl OsPointerSpeedSlider {
    pub(super) fn sync<V: 'static>(
        current: &mut Option<Self>,
        facts: &OsPointerSpeedFacts,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        if let Some(existing) = current.as_ref().filter(|slider| slider.key == facts.key) {
            existing.slider.sync(facts.speed, window, cx);
            return;
        }
        let slider = CommitSlider::new(
            SliderRange::new(MIN_SPEED, MAX_SPEED),
            facts.speed,
            cx,
            |_, speed, cx| {
                AppState::apply(cx, |state| {
                    state.commit_os_pointer_speed(Some(slider_to_speed(speed)))
                });
            },
        );
        *current = Some(Self {
            key: facts.key.clone(),
            slider,
        });
    }
}
pub(super) fn os_pointer_speed_section(
    facts: Option<OsPointerSpeedFacts>,
    slider: Option<&OsPointerSpeedSlider>,
    pal: Palette,
) -> Option<AnyElement> {
    let facts = facts?;
    let shown = slider.map_or(facts.speed, |slider| slider.slider.shown(facts.speed));
    let body = slider.map_or_else(
        || crate::ui::slider_row::disabled_track(pal).into_any_element(),
        |slider| {
            Slider::new(slider.slider.slider())
                .horizontal()
                .into_any_element()
        },
    );
    let toggle = crate::ui::components::Toggle::new("os-pointer-accel-toggle")
        .selected(facts.accel_enabled.unwrap_or(false))
        .label(tr!("pointer.os_pointer_accel"))
        .on_change(|selected, _window, cx| {
            let selected = *selected;
            AppState::apply(cx, |state| state.commit_os_pointer_accel(Some(selected)));
        });
    let mut row = SliderRow::new(tr!("pointer.os_pointer_speed"), body)
        .description(tr!("pointer.os_pointer_speed_description"))
        .value(format!("{:.2}", slider_to_speed(shown)), ["-1.00", "1.00"]);
    if !facts.configured {
        row = row.marker(tr!("default"));
    }
    Some(v_flex().gap_3().child(toggle).child(row).into_any_element())
}

fn slider_to_speed(value: i32) -> f32 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "slider values are bounded to -100..=100, exactly representable in f32"
    )]
    {
        value as f32 / 100.0
    }
}
fn speed_to_slider(speed: f32) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is clamped to the slider's small integer range before rounding"
    )]
    {
        (speed.clamp(-1.0, 1.0) * 100.0).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::{slider_to_speed, speed_to_slider};

    #[test]
    fn slider_positions_round_trip_to_the_compositor_factor() {
        for (position, factor) in [(-100, -1.0), (-48, -0.48), (0, 0.0), (48, 0.48), (100, 1.0)] {
            assert_eq!(slider_to_speed(position), factor);
            assert_eq!(speed_to_slider(factor), position);
        }
    }

    #[test]
    fn compositor_factors_outside_the_supported_range_are_clamped() {
        assert_eq!(speed_to_slider(-1.2), -100);
        assert_eq!(speed_to_slider(1.2), 100);
    }
}

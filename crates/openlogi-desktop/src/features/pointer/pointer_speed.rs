//! The pointer-speed row of the pointer-tuning card.
//!
//! A mouse without adjustable DPI, such as the MX Ergo or the M590, scales
//! its own motion with HID++ `0x2205`. For such a mouse this row takes the
//! place of the DPI slider and presets. The slider runs from 0 % to 100 %, as
//! in Logi Options+: 50 % is 1×, the ends are 0.125× and 3×, and equal steps
//! change the speed by equal ratios. A release goes through
//! [`AppState::commit_pointer_speed`], which persists the speed and writes
//! the mouse. When the mouse refuses the write, a note under the slider says
//! so, because the slider keeps showing the saved speed.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, Window, div};
use gpui_component::slider::Slider;
use openlogi_core::hid::PointerSpeed;

use crate::state::{AppState, DeviceKey, PointerSpeedLoad};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::slider_row::SliderRow;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::Palette;

/// What the row needs about the selected mouse.
pub(super) struct PointerSpeedFacts {
    key: DeviceKey,
    load: PointerSpeedLoad,
    /// The speed the slider shows: the saved one, else the reading.
    speed: Option<PointerSpeed>,
    reachable: bool,
    /// The mouse refused the last write and does not read the saved speed.
    refused: bool,
}

impl PointerSpeedFacts {
    /// The selected mouse's facts, or `None` when nothing is selected or the
    /// mouse does not scale its pointer with `0x2205`.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_pointer_speed_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_pointer_speed_load(),
            speed: state.current_pointer_speed(),
            reachable: record.route.is_some(),
            refused: state.current_pointer_speed_refused(),
        })
    }

    /// The speed to put the slider on, or `None` while the slider must not
    /// show: the mouse is offline, or its reading has not landed.
    fn slider_speed(&self) -> Option<PointerSpeed> {
        let ready = matches!(self.load, PointerSpeedLoad::Ready(_));
        self.speed.filter(|_| self.reachable && ready)
    }
}

/// The slider, kept across frames, with the mouse it was built for.
pub(super) struct PointerSpeedSlider {
    key: DeviceKey,
    slider: CommitSlider<PointerSpeed>,
}

impl PointerSpeedSlider {
    /// Keep `current` in step with `facts`: drop the slider when it must not
    /// show, build one for a newly selected mouse, or move the thumb when the
    /// speed changed underneath it.
    pub(super) fn sync<V: 'static>(
        current: &mut Option<Self>,
        facts: &PointerSpeedFacts,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let Some(speed) = facts.slider_speed() else {
            *current = None;
            return;
        };
        if let Some(existing) = current
            .as_ref()
            .filter(|existing| existing.key == facts.key)
        {
            existing.slider.sync(speed, window, cx);
            return;
        }
        let slider = CommitSlider::new(
            SliderRange::new(PointerSpeed::MIN, PointerSpeed::MAX),
            speed,
            cx,
            // `commit_pointer_speed` resolves the mouse when the thumb is
            // released, so a device switch mid-drag writes the mouse now on
            // screen.
            |_, speed, cx| AppState::apply(cx, |state| state.commit_pointer_speed(speed)),
        );
        *current = Some(Self {
            key: facts.key.clone(),
            slider,
        });
    }
}

/// The row: the speed as a percent and what the setting does, then the
/// slider or a status line, and a note when the mouse refused the speed.
pub(super) fn pointer_speed_section(
    facts: PointerSpeedFacts,
    slider: Option<&PointerSpeedSlider>,
    pal: Palette,
) -> AnyElement {
    let shown = facts
        .slider_speed()
        .map(|speed| slider.map_or(speed, |slider| slider.slider.shown(speed)));
    let PointerSpeedFacts {
        key,
        load,
        reachable,
        refused,
        ..
    } = facts;
    let body: AnyElement = match (load, slider) {
        _ if !reachable => status_line(
            tr!("pointer.device_offline_pointer_speed_is_unavailable"),
            pal,
        )
        .into_any_element(),
        (PointerSpeedLoad::Ready(_), Some(slider)) => Slider::new(slider.slider.slider())
            .horizontal()
            .into_any_element(),
        (PointerSpeedLoad::Ready(_), None)
        | (PointerSpeedLoad::Unknown | PointerSpeedLoad::Loading, _) => {
            status_line(tr!("pointer.reading_pointer_speed"), pal).into_any_element()
        }
        // Clickable in place, like the DPI retry: reselecting is a no-op for a
        // single-device gallery.
        (PointerSpeedLoad::Failed(_), _) => retry_line(
            "pointer-speed-retry",
            tr!("pointer.couldnt_read_pointer_speed_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_pointer_speed_read(&key)),
        )
        .into_any_element(),
        (PointerSpeedLoad::Unsupported(_), _) => {
            status_line(tr!("pointer.pointer_speed_unsupported"), pal).into_any_element()
        }
    };
    let percent = |speed: PointerSpeed| {
        tr!(
            "pointer.pointer_speed_percent",
            percent => speed.slider_percent()
        )
    };
    SliderRow::new(tr!("pointer.pointer_speed"), body)
        .description(tr!("pointer.pointer_speed_description"))
        .when_some(shown, |row, speed| {
            row.value(percent(speed), [percent(PointerSpeed::MAX)])
        })
        .when(refused && reachable, |row| {
            row.hint(
                div()
                    .debug_selector(|| "pointer-speed-refusal".into())
                    .child(tr!("pointer.pointer_speed_not_applied")),
            )
        })
        .into_any_element()
}

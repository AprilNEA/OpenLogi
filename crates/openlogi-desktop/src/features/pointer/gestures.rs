//! Per-device gesture tuning for the pointer-detail column.
//!
//! Two sliders over the selected device's gesture config: **sensitivity**
//! (how long a gesture button must be held, and how far it must travel,
//! before a swipe commits) and **axis bias** (how strongly Left/Right is
//! favored over Up/Down). Each release persists a device override through
//! [`AppState::commit_device_gesture_sensitivity`] /
//! [`AppState::commit_device_gesture_axis_bias`]; a device without one follows
//! the app-wide value from Settings → General. The agent turns both into
//! `SwipeThresholds` for the HID++ and OS-hook gesture paths.

use gpui::{
    Context, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use gpui_kit::component::{slider::Slider, v_flex};
use openlogi_core::config::{GestureAxisBias, GestureSensitivity};

use crate::state::{AppState, DeviceRecord, StateEvent, StateEvents};
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit};
use crate::ui::slider_row::SliderRow;

pub struct GesturePanel {
    /// The selected device's gesture sensitivity (device override, else the
    /// app-wide default).
    sensitivity: CommitSlider<GestureSensitivity>,
    /// The selected device's gesture axis bias (device override, else the
    /// app-wide default).
    axis_bias: CommitSlider<GestureAxisBias>,
    _state_obs: Subscription,
}

impl GesturePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sensitivity = CommitSlider::new(
            SliderRange::new(GestureSensitivity::MIN, GestureSensitivity::MAX),
            GestureSensitivity::DEFAULT,
            cx,
            |_, sensitivity, cx| {
                AppState::apply(cx, |state| {
                    state
                        .current_record()
                        .map(DeviceRecord::device_key)
                        .map_or_else(StateEvents::none, |key| {
                            state.commit_device_gesture_sensitivity(&key, sensitivity)
                        })
                });
            },
        );
        let axis_bias = CommitSlider::new(
            SliderRange::new(GestureAxisBias::MIN, GestureAxisBias::MAX),
            GestureAxisBias::DEFAULT,
            cx,
            |_, bias, cx| {
                AppState::apply(cx, |state| {
                    state
                        .current_record()
                        .map(DeviceRecord::device_key)
                        .map_or_else(StateEvents::none, |key| {
                            state.commit_device_gesture_axis_bias(&key, bias)
                        })
                });
            },
        );
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(event, StateEvent::DeviceConfigChanged(_))
        });
        Self {
            sensitivity,
            axis_bias,
            _state_obs: state_obs,
        }
    }
}

impl Render for GesturePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (sensitivity, axis_bias) = AppState::try_read(cx)
            .and_then(|state| {
                let key = &state.current_record()?.config_key;
                Some((
                    state.device_gesture_sensitivity(key),
                    state.device_gesture_axis_bias(key),
                ))
            })
            .unwrap_or((GestureSensitivity::DEFAULT, GestureAxisBias::DEFAULT));
        let sensitivity_row = slider_row(
            &mut self.sensitivity,
            sensitivity,
            [GestureSensitivity::MIN, GestureSensitivity::MAX],
            tr!("pointer.gesture_sensitivity"),
            tr!("pointer.gesture_sensitivity_description"),
            window,
            cx,
        );
        let axis_bias_row = slider_row(
            &mut self.axis_bias,
            axis_bias,
            [GestureAxisBias::MIN, GestureAxisBias::MAX],
            tr!("pointer.gesture_axis_bias"),
            tr!("pointer.gesture_axis_bias_description"),
            window,
            cx,
        );
        v_flex()
            .gap_4()
            .w_full()
            .child(sensitivity_row)
            .child(axis_bias_row)
    }
}

/// One slider row seated on `committed`, with its live value and a caption.
/// The value column is sized for the wider of `bounds`.
fn slider_row<T: SliderUnit + std::fmt::Display>(
    slider: &mut CommitSlider<T>,
    committed: T,
    bounds: [T; 2],
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<GesturePanel>,
) -> SliderRow {
    slider.sync(committed, window, cx);
    let display = slider.shown(committed);
    SliderRow::new(label, Slider::new(slider.slider()).horizontal())
        .value(display.to_string(), bounds.map(|b| b.to_string()))
        .description(description)
}

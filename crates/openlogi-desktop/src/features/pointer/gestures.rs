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
    Context, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    rgb,
};
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::config::{GestureAxisBias, GestureSensitivity};

use crate::state::{AppState, DeviceRecord, StateEvent, StateEvents};
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit};
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};

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
            tr!("pointer.gesture_sensitivity"),
            tr!("pointer.gesture_sensitivity_description"),
            window,
            cx,
        );
        let axis_bias_row = slider_row(
            &mut self.axis_bias,
            axis_bias,
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

/// One labelled slider seated on `committed`, with its value and a caption.
fn slider_row<T: SliderUnit + std::fmt::Display>(
    slider: &mut CommitSlider<T>,
    committed: T,
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<GesturePanel>,
) -> gpui::Div {
    let pal = theme::palette(cx);
    slider.sync(committed, window, cx);
    let display = slider.shown(committed);
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_baseline()
                .child(section_label(label, pal))
                .child(
                    div()
                        .text_body()
                        .text_color(rgb(ACCENT_BLUE))
                        .child(format!("{display}")),
                ),
        )
        .child(Slider::new(slider.slider()).horizontal())
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(description.into()),
        )
}

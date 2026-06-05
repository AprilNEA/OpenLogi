//! SmartShift wheel controls for the pointer-detail column.
//!
//! Three controls over the HID++ `0x2111` config: a wheel-mode segmented
//! control (free-spin ↔ ratchet), an auto-disengage **sensitivity** slider,
//! and a **permanent ratchet** toggle. The latter two only apply in ratchet
//! mode, so they grey out under free-spin.
//!
//! Unlike DPI presets, nothing here is written to `config.toml`: the device
//! persists wheel mode / threshold / torque in its own non-volatile memory, so
//! the panel only reads and writes the device (via
//! [`AppState::commit_smartshift`]). The current state is read lazily on the
//! same background-thread pattern as [`crate::components::dpi_panel`].

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, px, rgb,
};
use gpui_component::{
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_hid::{AUTO_DISENGAGE_PERMANENT, DeviceRoute, SmartShiftMode, SmartShiftStatus};

use crate::hardware::read_smartshift_status_blocking;
use crate::state::{AppState, SmartShiftLoad};
use crate::theme::{self, ACCENT_BLUE, Palette};

/// Friendly slider range for the `autoDisengage` threshold. The wire field is
/// `0x01`–`0xFE` (0.25 turn/s steps), but realistic scroll speeds sit well
/// inside 1–50 (≈12.5 turn/s) — Logitech's own default is ~16. A device
/// reporting a value above this is clamped for display; it is only rewritten
/// once the user actually drags the slider.
const THRESHOLD_MIN: u8 = 1;
const THRESHOLD_MAX: u8 = 50;
const DEFAULT_THRESHOLD: u8 = 16;

pub struct SmartShiftPanel {
    /// The auto-disengage threshold slider. Always constructed (range is
    /// builder-only); only *rendered* in ratchet, non-permanent mode.
    threshold: Entity<SliderState>,
    /// Last threshold pushed into the slider from the device, so toggling
    /// "permanent" off restores it and an external change re-seats the thumb —
    /// but an in-progress drag (tracked by `pending_threshold`) doesn't.
    last_threshold: u8,
    /// The live drag value, shown in the numeric label until release commits.
    pending_threshold: Option<u8>,
    _threshold_sub: Subscription,
    _state_obs: Subscription,
}

impl SmartShiftPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let threshold = cx.new(|_| {
            SliderState::new()
                .max(f32::from(THRESHOLD_MAX))
                .min(f32::from(THRESHOLD_MIN))
                .step(1.)
                .default_value(f32::from(DEFAULT_THRESHOLD))
        });
        // Drive the device only on release (a drag would stream a write burst);
        // Change just updates the numeric label.
        let threshold_sub =
            cx.subscribe(
                &threshold,
                |panel, _slider, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => {
                        panel.pending_threshold = Some(raw_to_threshold(value.start()));
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let t = raw_to_threshold(value.start());
                        panel.pending_threshold = None;
                        panel.last_threshold = t;
                        cx.update_global::<AppState, _>(|state, _| {
                            let torque = state
                                .current_smartshift_ready()
                                .map_or(0, |s| s.tunable_torque);
                            state.commit_smartshift(SmartShiftMode::Ratchet, t, torque);
                        });
                        cx.notify();
                    }
                },
            );
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
        Self {
            threshold,
            last_threshold: DEFAULT_THRESHOLD,
            pending_threshold: None,
            _threshold_sub: threshold_sub,
            _state_obs: state_obs,
        }
    }

    /// Kick off a one-shot SmartShift read for the active device when it hasn't
    /// been queried yet — same lazy, dedicated-OS-thread pattern as
    /// [`crate::components::dpi_panel::DpiPanel`].
    fn ensure_smartshift_load(cx: &mut Context<Self>) {
        let Some((key, route)) = smartshift_load_target(cx) else {
            return;
        };

        cx.update_global::<AppState, _>(|state, _| state.mark_smartshift_loading(&key));
        let key_for_reset = key.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let result = read_smartshift_status_blocking(&route);
            let _ = tx.send((key, route, result));
        });
        cx.spawn(async move |_panel, cx| match rx.await {
            Ok((key, route, result)) => {
                cx.update_global::<AppState, _>(|state, cx| {
                    state.store_smartshift_status(key, &route, result);
                    cx.refresh_windows();
                });
            }
            Err(_) => {
                cx.update_global::<AppState, _>(|state, cx| {
                    state.clear_smartshift_loading(&key_for_reset);
                    cx.refresh_windows();
                });
            }
        })
        .detach();
    }

    /// The interactive body shown once the device's SmartShift config resolves.
    fn ready_body(
        &mut self,
        status: SmartShiftStatus,
        window: &mut Window,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mode = status.mode;
        let permanent = status.auto_disengage == AUTO_DISENGAGE_PERMANENT;
        let torque = status.tunable_torque;
        let cur_auto = status.auto_disengage;
        let ratchet = matches!(mode, SmartShiftMode::Ratchet);
        let sensitivity_enabled = ratchet && !permanent;

        let committed = if permanent {
            self.last_threshold
        } else {
            clamp_threshold(status.auto_disengage)
        };
        // Re-seat the thumb on an external change (device re-read / mode switch),
        // never mid-drag, and keep `last_threshold` tracking the real value so a
        // permanent→off toggle can restore it.
        if !permanent && self.pending_threshold.is_none() && committed != self.last_threshold {
            self.last_threshold = committed;
            let v = f32::from(committed);
            self.threshold
                .update(cx, |s, cx| s.set_value(v, window, cx));
        }
        let display = self.pending_threshold.unwrap_or(committed);
        let restore_threshold = if permanent {
            self.last_threshold
        } else {
            committed
        };

        let mode_row = v_flex()
            .gap_2()
            .child(section_label(tr!("Wheel mode"), pal))
            .child(
                h_flex()
                    .gap_2()
                    .child(mode_pill(
                        tr!("Free spin"),
                        !ratchet,
                        SmartShiftMode::Free,
                        cur_auto,
                        torque,
                        pal,
                    ))
                    .child(mode_pill(
                        tr!("Ratchet"),
                        ratchet,
                        SmartShiftMode::Ratchet,
                        cur_auto,
                        torque,
                        pal,
                    )),
            );

        let value_color = if sensitivity_enabled {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_muted
        };
        let sensitivity_row = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("Sensitivity"), pal))
                    .child(
                        div()
                            .text_sm()
                            .text_color(value_color)
                            .child(format!("{display}")),
                    ),
            )
            .child(if sensitivity_enabled {
                Slider::new(&self.threshold).horizontal().into_any_element()
            } else {
                disabled_track(pal)
            })
            .child(div().text_xs().text_color(pal.text_muted).child(tr!(
                "Higher keeps the ratchet engaged longer before free-spin."
            )));

        let permanent_row = h_flex()
            .justify_between()
            .items_center()
            .child(
                v_flex()
                    .child(section_label(tr!("Permanent ratchet"), pal))
                    .child(
                        div()
                            .text_xs()
                            .text_color(pal.text_muted)
                            .child(tr!("Never auto-switch to free-spin.")),
                    ),
            )
            .child(permanent_toggle(
                permanent,
                ratchet,
                restore_threshold,
                torque,
                pal,
            ));

        v_flex()
            .gap_4()
            .w_full()
            .child(mode_row)
            .child(sensitivity_row)
            .child(permanent_row)
            .into_any_element()
    }
}

impl Render for SmartShiftPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Self::ensure_smartshift_load(cx);
        let pal = theme::palette(cx);

        let status = cx
            .try_global::<AppState>()
            .map_or(SmartShiftLoad::Unknown, AppState::current_smartshift_status);
        let reachable = cx
            .try_global::<AppState>()
            .and_then(AppState::current_record)
            .is_some_and(|r| r.route.is_some());

        let content: AnyElement = match status {
            SmartShiftLoad::Ready(s) => self.ready_body(s, window, pal, cx),
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown if !reachable => {
                status_line(tr!("Device offline — SmartShift unavailable."), pal)
            }
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown => {
                status_line(tr!("Reading SmartShift settings…"), pal)
            }
            SmartShiftLoad::Failed(_) => {
                retry_line(tr!("Couldn't read SmartShift — click to retry."), pal)
            }
            SmartShiftLoad::Unsupported(_) => {
                status_line(tr!("This device does not support SmartShift."), pal)
            }
        };

        v_flex().gap_3().w_full().child(content)
    }
}

fn smartshift_load_target(cx: &mut Context<SmartShiftPanel>) -> Option<(String, DeviceRoute)> {
    cx.try_global::<AppState>().and_then(|state| {
        if !state.current_smartshift_unqueried() {
            return None;
        }
        let record = state.current_record()?;
        Some((record.config_key.clone(), record.route.clone()?))
    })
}

/// A small muted section heading.
fn section_label(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .text_sm()
        .text_color(pal.text_muted)
        .child(text)
        .into_any_element()
}

/// One wheel-mode pill. Clicking it writes `target` while preserving the
/// device's current threshold + torque.
fn mode_pill(
    label: SharedString,
    selected: bool,
    target: SmartShiftMode,
    cur_auto: u8,
    torque: u8,
    pal: Palette,
) -> AnyElement {
    let id = match target {
        SmartShiftMode::Free => "smartshift-mode-free",
        SmartShiftMode::Ratchet => "smartshift-mode-ratchet",
    };
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if selected {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(if selected {
            pal.surface_hover
        } else {
            pal.surface
        })
        .text_sm()
        .text_color(if selected {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_primary
        })
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_smartshift(target, cur_auto, torque);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// The permanent-ratchet on/off pill. Disabled (muted, non-clickable) under
/// free-spin, where it has no meaning.
fn permanent_toggle(
    on: bool,
    enabled: bool,
    restore_threshold: u8,
    torque: u8,
    pal: Palette,
) -> AnyElement {
    let label = if on { tr!("On") } else { tr!("Off") };
    if !enabled {
        return div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .text_xs()
            .text_color(pal.text_muted)
            .child(label)
            .into_any_element();
    }
    div()
        .id("smartshift-permanent")
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if on {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .text_xs()
        .text_color(if on {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_muted
        })
        .cursor_pointer()
        .child(label)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                let next = if on {
                    restore_threshold
                } else {
                    AUTO_DISENGAGE_PERMANENT
                };
                state.commit_smartshift(SmartShiftMode::Ratchet, next, torque);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// A greyed bar standing in for the slider when sensitivity isn't adjustable.
fn disabled_track(pal: Palette) -> AnyElement {
    div()
        .w_full()
        .h(px(6.))
        .rounded_full()
        .bg(pal.border)
        .into_any_element()
}

fn status_line(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .text_sm()
        .text_color(pal.text_muted)
        .child(text)
        .into_any_element()
}

/// A `Failed`-state line that re-arms the SmartShift read on click — the only
/// recovery path when the carousel holds a single device.
fn retry_line(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .id("smartshift-retry")
        .text_sm()
        .text_color(rgb(ACCENT_BLUE))
        .hover(|s| s.text_color(pal.text_primary))
        .child(text)
        .on_click(|_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.retry_active_smartshift());
            cx.refresh_windows();
        })
        .into_any_element()
}

/// Round + clamp a raw slider read into the friendly threshold range.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is rounded and clamped into THRESHOLD_MIN..=THRESHOLD_MAX before the cast"
)]
fn raw_to_threshold(raw: f32) -> u8 {
    raw.round()
        .clamp(f32::from(THRESHOLD_MIN), f32::from(THRESHOLD_MAX)) as u8
}

/// Clamp a device-reported threshold into the slider's friendly range.
fn clamp_threshold(value: u8) -> u8 {
    value.clamp(THRESHOLD_MIN, THRESHOLD_MAX)
}

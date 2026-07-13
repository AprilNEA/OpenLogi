//! SmartShift wheel controls for the pointer-detail column.
//!
//! Three controls over the HID++ `0x2111` config: a wheel-mode segmented
//! control (free-spin ↔ ratchet), an auto-disengage **sensitivity** slider,
//! and a **permanent ratchet** toggle. The latter two only apply in ratchet
//! mode, so they grey out under free-spin.
//!
//! Each change is written to the device *and* persisted to `config.toml` (via
//! [`AppState::commit_smartshift`]): the device holds wheel mode / threshold /
//! torque in volatile RAM that resets on a power cycle (#189), so the agent
//! re-applies the saved config when the device reconnects. The current state is
//! read lazily on the same background-thread pattern as
//! [`crate::components::dpi_panel`].

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
use openlogi_core::config::{SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SMARTSHIFT_MIN_AUTO_DISENGAGE};
use openlogi_hid::{AUTO_DISENGAGE_PERMANENT, DeviceRoute, SmartShiftMode, SmartShiftStatus};

use crate::components::device_read::issue_device_read;
use crate::components::status::{retry_line, status_line};
use crate::state::{AppState, SmartShiftLoad};
use crate::theme::{self, ACCENT_BLUE, Palette, SelectableStyle};

/// Friendly slider range for the `autoDisengage` threshold. The wire field is
/// `0x01`–`0xFE` (0.25 turn/s steps); the slider exposes the usable band
/// [`SMARTSHIFT_MIN_AUTO_DISENGAGE`]–`50` (≈2–12.5 turn/s, default ~16).
/// Thresholds below the floor free-spin on everyday scrolling (#317), so the
/// floor and default are shared with the `openlogi-core` config heal. A device
/// reporting a value outside the band is normalised for display by
/// [`clamp_threshold`]; it is only rewritten once the user drags the slider.
const THRESHOLD_MIN: u8 = SMARTSHIFT_MIN_AUTO_DISENGAGE;
const THRESHOLD_MAX: u8 = 50;
const DEFAULT_THRESHOLD: u8 = SMARTSHIFT_AUTO_DISENGAGE_DEFAULT;

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
        Self::issue_smartshift_read(key, route, cx);
    }

    /// Re-read once after an optimistic write to confirm the device actually
    /// took it — a rejected / timed-out write would otherwise leave the panel
    /// showing a setting that never applied. No Loading marker, so the
    /// optimistic value stays on screen until the real state replaces it.
    fn ensure_smartshift_confirm(cx: &mut Context<Self>) {
        let Some((key, route)) =
            cx.update_global::<AppState, _>(|state, _| state.take_active_smartshift_confirm())
        else {
            return;
        };
        Self::issue_smartshift_read(key, route, cx);
    }

    /// Send a SmartShift read over IPC and store the typed result. Shared by the
    /// lazy initial load and the post-write confirm; the caller decides whether
    /// to set the Loading marker first. The agent returns the typed `WriteError`,
    /// so a permanent `FeatureUnsupported` reaches `store_smartshift_status`
    /// intact and the panel stops re-probing instead of retrying every reselect.
    fn issue_smartshift_read(key: String, route: DeviceRoute, cx: &mut Context<Self>) {
        issue_device_read(
            cx,
            key,
            route,
            crate::ipc_client::Command::ReadSmartShift,
            AppState::store_smartshift_status,
            AppState::clear_smartshift_loading,
        );
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
                        // `committed`, not `cur_auto`: when the cached value is
                        // `0xFF` (permanent ratchet) this resolves to the last
                        // real threshold, so switching to ratchet mode doesn't
                        // silently re-arm permanent ratchet behind the toggle.
                        committed,
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
        Self::ensure_smartshift_confirm(cx);
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
            SmartShiftLoad::Failed(_) => retry_line(
                "smartshift-retry",
                tr!("Couldn't read SmartShift — click to retry."),
                pal,
                |cx| {
                    cx.update_global::<AppState, _>(|state, _| state.retry_active_smartshift());
                    cx.refresh_windows();
                },
            ),
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
        .selected_border(selected, pal)
        .bg(pal.surface)
        .selected_fill(selected)
        .text_sm()
        .text_color(pal.text_primary)
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
        .selected_border(on, pal)
        .selected_fill(on)
        .text_xs()
        .text_color(if on { pal.text_primary } else { pal.text_muted })
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

/// Map a device-reported threshold into the slider's friendly band for display.
///
/// A non-permanent auto-disengage below [`THRESHOLD_MIN`] — including the `0`
/// "do not change"/unset sentinel — releases the wheel into free-spin on the
/// gentlest scroll (#317), so it must never seed the slider or the
/// permanent-ratchet restore at that runaway value. Such values are normalised
/// to the default (matching the `openlogi-core` config heal); values above the
/// band clamp down to [`THRESHOLD_MAX`]. (`0xFF` permanent ratchet never reaches
/// here — the caller handles it before clamping.)
fn clamp_threshold(value: u8) -> u8 {
    if value < THRESHOLD_MIN {
        DEFAULT_THRESHOLD
    } else {
        value.min(THRESHOLD_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_THRESHOLD, THRESHOLD_MAX, THRESHOLD_MIN, clamp_threshold};

    #[test]
    fn clamp_threshold_heals_sub_floor_to_default() {
        // 0 (the firmware "do not change" sentinel) and any sub-floor value
        // used to seed the slider / permanent-ratchet restore with a runaway
        // free-spin threshold (#317); they normalise to the default instead.
        assert_eq!(clamp_threshold(0), DEFAULT_THRESHOLD);
        assert_eq!(clamp_threshold(1), DEFAULT_THRESHOLD);
        assert_eq!(clamp_threshold(THRESHOLD_MIN - 1), DEFAULT_THRESHOLD);
    }

    #[test]
    fn clamp_threshold_keeps_in_band_values_and_clamps_high() {
        assert_eq!(clamp_threshold(THRESHOLD_MIN), THRESHOLD_MIN);
        assert_eq!(clamp_threshold(16), 16);
        assert_eq!(clamp_threshold(200), THRESHOLD_MAX);
    }
}

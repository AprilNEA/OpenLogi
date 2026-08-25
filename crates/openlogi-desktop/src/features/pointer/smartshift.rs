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
//! re-applies the saved config when the device reconnects. [`AppState`] reads
//! the current value through the agent when selection/inventory lifecycle
//! events make a device active; this view only consumes the resulting cache.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px, rgb,
};
use gpui_component::{
    Disableable as _, Selectable as _,
    button::Button,
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::config::{
    SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SMARTSHIFT_MIN_AUTO_DISENGAGE, ThumbwheelSensitivity,
};
use openlogi_core::hid::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold,
};

use crate::state::{AppState, DeviceKey, SmartShiftLoad, SmartShiftWriteStatus, StateEvent};
use crate::ui::components::Toggle;
use crate::ui::section::section_label;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

/// Friendly slider range for the `autoDisengage` threshold. The wire field is
/// `0x01`–`0xFE` (0.25 turn/s steps); the slider exposes the usable band
/// [`SMARTSHIFT_MIN_AUTO_DISENGAGE`]–`50` (≈2–12.5 turn/s, default ~16).
/// Thresholds below the floor free-spin on everyday scrolling (#317), so the
/// floor and default are shared with the `openlogi-core` config contract. A device
/// reporting a value outside the band is normalised for display by
/// [`clamp_threshold`]; it is only rewritten once the user drags the slider.
const THRESHOLD_MIN: SmartShiftThreshold = SMARTSHIFT_MIN_AUTO_DISENGAGE;
const THRESHOLD_MAX: SmartShiftThreshold = match SmartShiftThreshold::try_new(50) {
    Ok(value) => value,
    Err(_) => panic!("valid maximum SmartShift slider threshold"),
};
const DEFAULT_THRESHOLD: SmartShiftThreshold = SMARTSHIFT_AUTO_DISENGAGE_DEFAULT;

pub struct SmartShiftPanel {
    /// The auto-disengage threshold slider. Always constructed (range is
    /// builder-only); only *rendered* in ratchet, non-permanent mode.
    threshold: Entity<SliderState>,
    /// Last threshold pushed into the slider from the device, so toggling
    /// "permanent" off restores it and an external change re-seats the thumb —
    /// but an in-progress drag (tracked by `pending_threshold`) doesn't.
    last_threshold: SmartShiftThreshold,
    /// The live drag value, shown in the numeric label until release commits.
    pending_threshold: Option<SmartShiftThreshold>,
    _threshold_sub: Subscription,
    /// The per-device thumb-wheel sensitivity slider (device override; devices
    /// without one follow the app-wide default from Settings → General).
    wheel_sensitivity: Entity<SliderState>,
    /// Last committed sensitivity, to re-seat the thumb on a device switch.
    last_wheel_sensitivity: ThumbwheelSensitivity,
    /// Live drag value shown in the numeric label until release commits.
    pending_wheel_sensitivity: Option<ThumbwheelSensitivity>,
    _wheel_sensitivity_sub: Subscription,
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
                        panel.pending_threshold = Some(threshold_from_slider(value.start()));
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let threshold = threshold_from_slider(value.start());
                        panel.pending_threshold = None;
                        panel.last_threshold = threshold;
                        let status =
                            AppState::try_read(cx).and_then(AppState::current_smartshift_ready);
                        if let Some(status) = status {
                            AppState::update_smartshift(
                                cx,
                                SmartShiftStatus {
                                    mode: SmartShiftMode::Ratchet,
                                    auto_disengage: SmartShiftAutoDisengage::Threshold(threshold),
                                    ..status
                                },
                            );
                        }
                        cx.notify();
                    }
                },
            );
        let wheel_sensitivity = cx.new(|_| {
            SliderState::new()
                .min(f32::from(ThumbwheelSensitivity::MIN))
                .max(f32::from(ThumbwheelSensitivity::MAX))
                .step(1.)
                .default_value(f32::from(ThumbwheelSensitivity::DEFAULT))
        });
        let wheel_sensitivity_sub = cx.subscribe(
            &wheel_sensitivity,
            |panel, _slider, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => {
                    panel.pending_wheel_sensitivity =
                        Some(ThumbwheelSensitivity::from_rounded(value.start()));
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    let sensitivity = ThumbwheelSensitivity::from_rounded(value.start());
                    panel.pending_wheel_sensitivity = None;
                    panel.last_wheel_sensitivity = sensitivity;
                    AppState::update(cx, |state, cx| {
                        let record = state
                            .current_record()
                            .map(|record| (record.config_key.clone(), record.device_key()));
                        if let Some((config_key, event_key)) = record {
                            state.set_device_thumbwheel_sensitivity(&config_key, sensitivity);
                            cx.emit(StateEvent::DeviceConfigChanged(event_key));
                        }
                    });
                    cx.notify();
                }
            },
        );
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::SmartShiftChanged(key) | StateEvent::DeviceConfigChanged(key) => {
                    AppState::try_read(cx)
                        .and_then(AppState::current_record)
                        .is_some_and(|record| record.device_key() == *key)
                }
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            threshold,
            last_threshold: DEFAULT_THRESHOLD,
            pending_threshold: None,
            _threshold_sub: threshold_sub,
            wheel_sensitivity,
            last_wheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
            pending_wheel_sensitivity: None,
            _wheel_sensitivity_sub: wheel_sensitivity_sub,
            _state_obs: state_obs,
        }
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
        let permanent = status.auto_disengage.is_permanent();
        let ratchet = matches!(mode, SmartShiftMode::Ratchet);
        let sensitivity_enabled = ratchet && !permanent;

        let committed = status
            .auto_disengage
            .threshold()
            .map_or(self.last_threshold, clamp_threshold);
        // Re-seat the thumb on an external change (device re-read / mode switch),
        // never mid-drag, and keep `last_threshold` tracking the real value so a
        // permanent→off toggle can restore it.
        if !permanent && self.pending_threshold.is_none() && committed != self.last_threshold {
            self.last_threshold = committed;
            self.threshold
                .update(cx, |s, cx| s.set_value(f32::from(committed), window, cx));
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
                        SmartShiftStatus {
                            mode: SmartShiftMode::Free,
                            ..status
                        },
                        pal,
                    ))
                    .child(mode_pill(
                        tr!("Ratchet"),
                        ratchet,
                        // `committed`, not the current setting: when the cached value is
                        // `0xFF` (permanent ratchet) this resolves to the last
                        // real threshold, so switching to ratchet mode doesn't
                        // silently re-arm permanent ratchet behind the toggle.
                        SmartShiftStatus {
                            mode: SmartShiftMode::Ratchet,
                            auto_disengage: SmartShiftAutoDisengage::Threshold(committed),
                            ..status
                        },
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
                            .text_body()
                            .text_color(value_color)
                            .child(format!("{display}")),
                    ),
            )
            .child(if sensitivity_enabled {
                Slider::new(&self.threshold).horizontal().into_any_element()
            } else {
                disabled_track(pal)
            })
            .child(div().text_caption().text_color(pal.text_muted).child(tr!(
                "Higher keeps the ratchet engaged longer before free-spin."
            )));

        let wheel_row = self.wheel_sensitivity_row(window, pal, cx);

        let permanent_row = permanent_row(permanent, ratchet, restore_threshold, status, pal);

        v_flex()
            .gap_4()
            .w_full()
            .child(mode_row)
            .child(sensitivity_row)
            .child(permanent_row)
            .child(wheel_row)
            .into_any_element()
    }
}

impl SmartShiftPanel {
    /// The per-device thumb-wheel sensitivity row: label, live value, slider.
    /// Reads the selected device's effective value and re-seats the thumb on a
    /// device switch / external config change, never mid-drag.
    fn wheel_sensitivity_row(
        &mut self,
        window: &mut Window,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let committed = AppState::try_read(cx)
            .and_then(|state| {
                state
                    .current_record()
                    .map(|r| state.device_thumbwheel_sensitivity(&r.config_key))
            })
            .unwrap_or(ThumbwheelSensitivity::DEFAULT);
        if self.pending_wheel_sensitivity.is_none() && committed != self.last_wheel_sensitivity {
            self.last_wheel_sensitivity = committed;
            self.wheel_sensitivity.update(cx, |s, cx| {
                s.set_value(f32::from(committed), window, cx);
            });
        }
        let display = self.pending_wheel_sensitivity.unwrap_or(committed);
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("Thumb Wheel Sensitivity"), pal))
                    .child(
                        div()
                            .text_body()
                            .text_color(rgb(ACCENT_BLUE))
                            .child(format!("{display}")),
                    ),
            )
            .child(
                Slider::new(&self.wheel_sensitivity)
                    .horizontal()
                    .into_any_element(),
            )
            .into_any_element()
    }
}

impl Render for SmartShiftPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        let (key, status) = AppState::try_read(cx)
            .and_then(|state| {
                let key = state.current_record()?.device_key();
                Some((Some(key.clone()), state.reads.smartshift_status(&key)))
            })
            .unwrap_or((None, SmartShiftLoad::Unknown));
        let write_status =
            AppState::try_read(cx).and_then(AppState::current_smartshift_write_status);
        let reachable = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .is_some_and(|r| r.route.is_some());

        let show_write_status = matches!(status, SmartShiftLoad::Ready(_));
        let content: AnyElement = match status {
            SmartShiftLoad::Ready(s) => self.ready_body(*s, window, pal, cx),
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown if !reachable => {
                status_line(tr!("Device offline — SmartShift unavailable."), pal).into_any_element()
            }
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown => {
                status_line(tr!("Reading SmartShift settings…"), pal).into_any_element()
            }
            SmartShiftLoad::Failed(_) => retry_line(
                "smartshift-retry",
                tr!("Couldn't read SmartShift — click to retry."),
                pal,
                retry_smartshift_closure(key.clone()),
            )
            .into_any_element(),
            SmartShiftLoad::Unsupported(_) => {
                status_line(tr!("This device does not support SmartShift."), pal).into_any_element()
            }
        };

        let feedback = show_write_status
            .then(|| smartshift_write_feedback(write_status, key, pal))
            .flatten();
        v_flex().gap_3().w_full().child(content).children(feedback)
    }
}

/// A retry action bound to `key`, or a no-op when there is no active device.
fn retry_smartshift_closure(key: Option<DeviceKey>) -> impl Fn(&mut App) + 'static {
    move |cx| {
        if let Some(key) = &key {
            AppState::retry_smartshift_read(cx, key.clone());
        }
    }
}

fn smartshift_write_feedback(
    status: Option<SmartShiftWriteStatus>,
    key: Option<DeviceKey>,
    pal: Palette,
) -> Option<AnyElement> {
    match status {
        Some(SmartShiftWriteStatus::Applying { .. }) => {
            Some(status_line(tr!("Reading SmartShift settings…"), pal).into_any_element())
        }
        Some(SmartShiftWriteStatus::Confirmed) => {
            Some(status_line(tr!("Done"), pal).into_any_element())
        }
        Some(SmartShiftWriteStatus::Failed) => Some(
            retry_line(
                "smartshift-confirm-retry",
                tr!("Couldn't read SmartShift — click to retry."),
                pal,
                retry_smartshift_closure(key),
            )
            .into_any_element(),
        ),
        None => None,
    }
}

/// The "Permanent ratchet" label + toggle row.
fn permanent_row(
    permanent: bool,
    ratchet: bool,
    restore_threshold: SmartShiftThreshold,
    status: SmartShiftStatus,
    pal: Palette,
) -> gpui::Div {
    h_flex()
        .justify_between()
        .items_center()
        .child(
            v_flex()
                .child(section_label(tr!("Permanent ratchet"), pal))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("Never auto-switch to free-spin.")),
                ),
        )
        .child(
            Toggle::new("smartshift-permanent")
                .selected(permanent)
                .disabled(!ratchet)
                .on_change(move |permanent, _window, cx| {
                    let auto_disengage = if *permanent {
                        SmartShiftAutoDisengage::Permanent
                    } else {
                        SmartShiftAutoDisengage::Threshold(restore_threshold)
                    };
                    AppState::update_smartshift(
                        cx,
                        SmartShiftStatus {
                            mode: SmartShiftMode::Ratchet,
                            auto_disengage,
                            ..status
                        },
                    );
                }),
        )
}

/// One wheel-mode pill. Clicking it writes `target` while preserving the
/// device's current threshold + torque.
fn mode_pill(
    label: SharedString,
    selected: bool,
    status: SmartShiftStatus,
    _pal: Palette,
) -> AnyElement {
    let id = match status.mode {
        SmartShiftMode::Free => "smartshift-mode-free",
        SmartShiftMode::Ratchet => "smartshift-mode-ratchet",
    };
    Button::new(id)
        .compact()
        .label(label)
        .selected(selected)
        .on_click(move |_event, _window, cx| {
            AppState::update_smartshift(cx, status);
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
fn threshold_from_slider(raw: f32) -> SmartShiftThreshold {
    SmartShiftThreshold::from_rounded(raw).clamp(THRESHOLD_MIN, THRESHOLD_MAX)
}

/// Map a device-reported threshold into the slider's friendly band for display.
///
/// A non-permanent auto-disengage below [`THRESHOLD_MIN`] releases the wheel
/// into free-spin on the gentlest scroll (#317), so it must never seed the
/// slider or permanent-ratchet restore at that runaway value. Such values are
/// normalised to the default; values above the band clamp to [`THRESHOLD_MAX`].
fn clamp_threshold(value: SmartShiftThreshold) -> SmartShiftThreshold {
    if value < THRESHOLD_MIN {
        DEFAULT_THRESHOLD
    } else {
        value.min(THRESHOLD_MAX)
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::SmartShiftThreshold;

    use super::{DEFAULT_THRESHOLD, THRESHOLD_MAX, THRESHOLD_MIN, clamp_threshold};

    #[test]
    fn clamp_threshold_heals_sub_floor_to_default() {
        // A sub-floor device value used to seed the slider / permanent-ratchet
        // restore with a runaway free-spin threshold (#317).
        assert_eq!(
            clamp_threshold(SmartShiftThreshold::from_rounded(1.0)),
            DEFAULT_THRESHOLD
        );
        assert_eq!(
            clamp_threshold(SmartShiftThreshold::from_rounded(7.0)),
            DEFAULT_THRESHOLD
        );
    }

    #[test]
    fn clamp_threshold_keeps_in_band_values_and_clamps_high() {
        assert_eq!(clamp_threshold(THRESHOLD_MIN), THRESHOLD_MIN);
        let default = SmartShiftThreshold::from_rounded(16.0);
        assert_eq!(clamp_threshold(default), default);
        assert_eq!(
            clamp_threshold(SmartShiftThreshold::from_rounded(200.0)),
            THRESHOLD_MAX
        );
    }
}

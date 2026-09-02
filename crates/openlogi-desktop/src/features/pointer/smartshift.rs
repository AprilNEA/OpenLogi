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

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, px, rgb,
};
use gpui_component::{
    Disableable as _, Selectable as _,
    button::Button,
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::config::{
    GestureAxisBias, GestureSensitivity, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT,
    SMARTSHIFT_MIN_AUTO_DISENGAGE, ThumbwheelSensitivity,
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
    /// The per-device gesture sensitivity slider (device override; devices
    /// without one follow the app-wide default from Settings → General).
    gesture_sensitivity: Entity<SliderState>,
    /// Last committed gesture sensitivity, to re-seat the thumb on a device switch.
    last_gesture_sensitivity: GestureSensitivity,
    /// Live drag value shown in the numeric label until release commits.
    pending_gesture_sensitivity: Option<GestureSensitivity>,
    _gesture_sensitivity_sub: Subscription,
    /// The per-device gesture axis bias slider (device override; devices
    /// without one follow the app-wide default from Settings → General).
    gesture_bias: Entity<SliderState>,
    /// Last committed gesture axis bias, to re-seat the thumb on a device switch.
    last_gesture_bias: GestureAxisBias,
    /// Live drag value shown in the numeric label until release commits.
    pending_gesture_bias: Option<GestureAxisBias>,
    _gesture_bias_sub: Subscription,
    _state_obs: Subscription,
}

impl SmartShiftPanel {
    #[expect(
        clippy::too_many_lines,
        reason = "wires threshold, wheel sensitivity, gesture sensitivity, and gesture axis bias slider state subscriptions"
    )]
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
        let gesture_sensitivity = cx.new(|_| {
            SliderState::new()
                .min(f32::from(GestureSensitivity::MIN))
                .max(f32::from(GestureSensitivity::MAX))
                .step(1.)
                .default_value(f32::from(GestureSensitivity::DEFAULT))
        });
        let gesture_sensitivity_sub = cx.subscribe(
            &gesture_sensitivity,
            |panel, _slider, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => {
                    panel.pending_gesture_sensitivity =
                        Some(GestureSensitivity::from_rounded(value.start()));
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    let sensitivity = GestureSensitivity::from_rounded(value.start());
                    panel.pending_gesture_sensitivity = None;
                    panel.last_gesture_sensitivity = sensitivity;
                    AppState::update(cx, |state, cx| {
                        let record = state
                            .current_record()
                            .map(|record| (record.config_key.clone(), record.device_key()));
                        if let Some((config_key, event_key)) = record {
                            state.set_device_gesture_sensitivity(&config_key, sensitivity);
                            cx.emit(StateEvent::DeviceConfigChanged(event_key));
                        }
                    });
                    cx.notify();
                }
            },
        );
        let gesture_bias = cx.new(|_| {
            SliderState::new()
                .min(f32::from(GestureAxisBias::MIN))
                .max(f32::from(GestureAxisBias::MAX))
                .step(1.)
                .default_value(f32::from(GestureAxisBias::DEFAULT))
        });
        let gesture_bias_sub = cx.subscribe(
            &gesture_bias,
            |panel, _slider, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => {
                    panel.pending_gesture_bias = Some(GestureAxisBias::from_rounded(value.start()));
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    let bias = GestureAxisBias::from_rounded(value.start());
                    panel.pending_gesture_bias = None;
                    panel.last_gesture_bias = bias;
                    AppState::update(cx, |state, cx| {
                        let record = state
                            .current_record()
                            .map(|record| (record.config_key.clone(), record.device_key()));
                        if let Some((config_key, event_key)) = record {
                            state.set_device_gesture_axis_bias(&config_key, bias);
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
            gesture_sensitivity,
            last_gesture_sensitivity: GestureSensitivity::DEFAULT,
            pending_gesture_sensitivity: None,
            _gesture_sensitivity_sub: gesture_sensitivity_sub,
            gesture_bias,
            last_gesture_bias: GestureAxisBias::DEFAULT,
            pending_gesture_bias: None,
            _gesture_bias_sub: gesture_bias_sub,
            _state_obs: state_obs,
        }
    }

    /// The interactive body shown once the device's SmartShift config resolves.
    fn ready_body(
        &mut self,
        status: SmartShiftStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
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
            .child(section_label(tr!("pointer.wheel_mode"), pal))
            .child(
                h_flex()
                    .gap_2()
                    .child(mode_pill(
                        tr!("pointer.free_spin"),
                        !ratchet,
                        SmartShiftStatus {
                            mode: SmartShiftMode::Free,
                            ..status
                        },
                    ))
                    .child(mode_pill(
                        tr!("pointer.ratchet"),
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
                    )),
            );

        let sensitivity_row =
            self.smartshift_sensitivity_row(sensitivity_enabled, display, pal, cx);

        let wheel_row = self.wheel_sensitivity_row(window, cx);
        let gesture_row = self.gesture_sensitivity_row(window, cx);
        let gesture_bias_row = self.gesture_axis_bias_row(window, cx);

        let permanent_row = permanent_row(permanent, ratchet, restore_threshold, status, pal);

        v_flex()
            .gap_4()
            .w_full()
            .child(mode_row)
            .child(sensitivity_row)
            .child(permanent_row)
            .child(wheel_row)
            .child(gesture_row)
            .child(gesture_bias_row)
    }

    /// The auto-disengage sensitivity row: label, live threshold, slider with
    /// double-click reset to default.
    fn smartshift_sensitivity_row(
        &self,
        sensitivity_enabled: bool,
        display: SmartShiftThreshold,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let value_color = if sensitivity_enabled {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_muted
        };
        let handle = self.threshold.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.sensitivity"), pal))
                    .child(
                        div()
                            .text_body()
                            .text_color(value_color)
                            .child(format!("{display}")),
                    ),
            )
            .when(sensitivity_enabled, |row| {
                row.child(
                    div()
                        .capture_any_mouse_down(cx.listener(
                            move |_this, event: &MouseDownEvent, window, cx| {
                                if event.button == MouseButton::Left && event.click_count == 2 {
                                    cx.stop_propagation();
                                    handle.update(cx, |s, cx| {
                                        s.set_value(f32::from(DEFAULT_THRESHOLD), window, cx);
                                    });
                                    let status = AppState::try_read(cx)
                                        .and_then(AppState::current_smartshift_ready);
                                    if let Some(status) = status {
                                        AppState::update_smartshift(
                                            cx,
                                            SmartShiftStatus {
                                                mode: SmartShiftMode::Ratchet,
                                                auto_disengage: SmartShiftAutoDisengage::Threshold(
                                                    DEFAULT_THRESHOLD,
                                                ),
                                                ..status
                                            },
                                        );
                                    }
                                }
                            },
                        ))
                        .child(Slider::new(&self.threshold).horizontal()),
                )
            })
            .when(!sensitivity_enabled, |row| row.child(disabled_track(pal)))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.smartshift_sensitivity_description")),
            )
    }
}

impl SmartShiftPanel {
    /// The per-device gesture sensitivity row: label, live value, slider.
    /// Reads the selected device's effective value and re-seats the thumb on a
    /// device switch / external config change, never mid-drag.
    fn gesture_sensitivity_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        let committed = AppState::try_read(cx)
            .and_then(|state| {
                state
                    .current_record()
                    .map(|r| state.device_gesture_sensitivity(&r.config_key))
            })
            .unwrap_or(GestureSensitivity::DEFAULT);
        if self.pending_gesture_sensitivity.is_none() && committed != self.last_gesture_sensitivity
        {
            self.last_gesture_sensitivity = committed;
            self.gesture_sensitivity.update(cx, |s, cx| {
                s.set_value(f32::from(committed), window, cx);
            });
        }
        let display = self.pending_gesture_sensitivity.unwrap_or(committed);
        let handle = self.gesture_sensitivity.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.gesture_sensitivity"), pal))
                    .child(
                        div()
                            .text_body()
                            .text_color(rgb(ACCENT_BLUE))
                            .child(format!("{display}")),
                    ),
            )
            .child(
                div()
                    .capture_any_mouse_down(cx.listener(
                        move |_this, event: &MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left && event.click_count == 2 {
                                cx.stop_propagation();
                                handle.update(cx, |s, cx| {
                                    s.set_value(f32::from(GestureSensitivity::DEFAULT), window, cx);
                                });
                                if let Some(key) = AppState::try_read(cx)
                                    .and_then(|s| s.current_record().map(|r| r.config_key.clone()))
                                {
                                    AppState::update(cx, |state, cx| {
                                        state.set_device_gesture_sensitivity(
                                            &key,
                                            GestureSensitivity::DEFAULT,
                                        );
                                        cx.emit(StateEvent::SettingsChanged);
                                    });
                                }
                            }
                        },
                    ))
                    .child(Slider::new(&self.gesture_sensitivity).horizontal()),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.gesture_sensitivity_description")),
            )
    }

    /// The per-device gesture axis bias row: label, live value, slider.
    /// Reads the selected device's effective value and re-seats the thumb on a
    /// device switch / external config change, never mid-drag.
    fn gesture_axis_bias_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        let committed = AppState::try_read(cx)
            .and_then(|state| {
                state
                    .current_record()
                    .map(|r| state.device_gesture_axis_bias(&r.config_key))
            })
            .unwrap_or(GestureAxisBias::DEFAULT);
        if self.pending_gesture_bias.is_none() && committed != self.last_gesture_bias {
            self.last_gesture_bias = committed;
            self.gesture_bias.update(cx, |s, cx| {
                s.set_value(f32::from(committed), window, cx);
            });
        }
        let display = self.pending_gesture_bias.unwrap_or(committed);
        let raw = i8::from(display);
        let label = match raw.cmp(&0) {
            std::cmp::Ordering::Less => {
                format!("{} ({})", tr!("common.horizontal"), raw.abs())
            }
            std::cmp::Ordering::Greater => format!("{} ({})", tr!("common.vertical"), raw),
            std::cmp::Ordering::Equal => tr!("common.neutral").to_string(),
        };
        let handle = self.gesture_bias.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.gesture_axis_bias"), pal))
                    .child(div().text_body().text_color(rgb(ACCENT_BLUE)).child(label)),
            )
            .child(
                div()
                    .capture_any_mouse_down(cx.listener(
                        move |_this, event: &MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left && event.click_count == 2 {
                                cx.stop_propagation();
                                handle.update(cx, |s, cx| {
                                    s.set_value(f32::from(GestureAxisBias::DEFAULT), window, cx);
                                });
                                if let Some(key) = AppState::try_read(cx)
                                    .and_then(|s| s.current_record().map(|r| r.config_key.clone()))
                                {
                                    AppState::update(cx, |state, cx| {
                                        state.set_device_gesture_axis_bias(
                                            &key,
                                            GestureAxisBias::DEFAULT,
                                        );
                                        cx.emit(StateEvent::SettingsChanged);
                                    });
                                }
                            }
                        },
                    ))
                    .child(Slider::new(&self.gesture_bias).horizontal()),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.gesture_axis_bias_description")),
            )
    }

    /// The per-device thumb-wheel sensitivity row: label, live value, slider.
    /// Reads the selected device's effective value and re-seats the thumb on a
    /// device switch / external config change, never mid-drag.
    fn wheel_sensitivity_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
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
        let handle = self.wheel_sensitivity.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.thumb_wheel_sensitivity"), pal))
                    .child(
                        div()
                            .text_body()
                            .text_color(rgb(ACCENT_BLUE))
                            .child(format!("{display}")),
                    ),
            )
            .child(
                div()
                    .capture_any_mouse_down(cx.listener(
                        move |_this, event: &MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left && event.click_count == 2 {
                                cx.stop_propagation();
                                handle.update(cx, |s, cx| {
                                    s.set_value(
                                        f32::from(ThumbwheelSensitivity::DEFAULT),
                                        window,
                                        cx,
                                    );
                                });
                                if let Some(key) = AppState::try_read(cx)
                                    .and_then(|s| s.current_record().map(|r| r.config_key.clone()))
                                {
                                    AppState::update(cx, |state, cx| {
                                        state.set_device_thumbwheel_sensitivity(
                                            &key,
                                            ThumbwheelSensitivity::DEFAULT,
                                        );
                                        cx.emit(StateEvent::SettingsChanged);
                                    });
                                }
                            }
                        },
                    ))
                    .child(Slider::new(&self.wheel_sensitivity).horizontal()),
            )
    }
}

impl Render for SmartShiftPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        let (key, status) = AppState::try_read(cx)
            .and_then(|state| {
                let key = state.current_record()?.device_key();
                Some((Some(key.clone()), state.smartshift_status_for(&key)))
            })
            .unwrap_or((None, SmartShiftLoad::Unknown));
        let write_status =
            AppState::try_read(cx).and_then(AppState::current_smartshift_write_status);
        let reachable = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .is_some_and(|r| r.route.is_some());

        let show_write_status = matches!(status, SmartShiftLoad::Ready(_));
        let content: AnyElement = match status {
            SmartShiftLoad::Ready(s) => self.ready_body(*s, window, cx).into_any_element(),
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown if !reachable => {
                status_line(tr!("pointer.device_offline_smartshift_is_unavailable"), pal)
                    .into_any_element()
            }
            SmartShiftLoad::Loading | SmartShiftLoad::Unknown => {
                status_line(tr!("pointer.reading_smartshift_settings"), pal).into_any_element()
            }
            SmartShiftLoad::Failed(_) => retry_line(
                "smartshift-retry",
                tr!("pointer.couldnt_read_smartshift_click_to_retry"),
                pal,
                retry_smartshift_closure(key.clone()),
            )
            .into_any_element(),
            SmartShiftLoad::Unsupported(_) => {
                status_line(tr!("pointer.this_device_does_not_support_smartshift"), pal)
                    .into_any_element()
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
            Some(status_line(tr!("pointer.reading_smartshift_settings"), pal).into_any_element())
        }
        Some(SmartShiftWriteStatus::Confirmed) => {
            Some(status_line(tr!("common.done"), pal).into_any_element())
        }
        Some(SmartShiftWriteStatus::Failed) => Some(
            retry_line(
                "smartshift-confirm-retry",
                tr!("pointer.couldnt_read_smartshift_click_to_retry"),
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
                .child(section_label(tr!("pointer.permanent_ratchet"), pal))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("pointer.never_auto_switch_to_free_spin")),
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
fn mode_pill(label: SharedString, selected: bool, status: SmartShiftStatus) -> impl IntoElement {
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
}

/// A greyed bar standing in for the slider when sensitivity isn't adjustable.
fn disabled_track(pal: Palette) -> gpui::Div {
    div().w_full().h(px(6.)).rounded_full().bg(pal.border)
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

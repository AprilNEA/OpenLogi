//! Camera controls and profiles.
//!
//! The GUI saves desired controls and profiles. The agent applies native UVC
//! changes and publishes measured state, including partial-write failures.

use gpui::{
    App, Context, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div,
};
use gpui_component::{Disableable as _, v_flex};
use openlogi_camera::{AutoToggle, CameraControl, CameraState, ControlRange};
use openlogi_core::config::CameraControls;
use openlogi_core::peripheral::{ApplicationStatus, Capability, ConnectionStatus, OperationStatus};

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::section::section_label;
use crate::ui::theme::{self, Typography as _};

mod rows;
use rows::{
    control_label, control_row, from_slider, profiles_row, reset_button, section_indices, to_slider,
};

/// Built-in profiles: `values` are fractions of each control's own range, so
/// they scale to whatever the camera reports. Auto modes all engage — the
/// point of a preset is a good picture without babysitting.
const BUILTIN_PROFILES: [BuiltinProfile; 3] = [
    BuiltinProfile {
        id: "default",
        values: &[],
    },
    BuiltinProfile {
        id: "streaming",
        values: &[
            (CameraControl::Brightness, 0.50),
            (CameraControl::Contrast, 0.58),
            (CameraControl::Saturation, 0.62),
            (CameraControl::Sharpness, 0.60),
        ],
    },
    BuiltinProfile {
        id: "video_call",
        values: &[
            (CameraControl::Brightness, 0.55),
            (CameraControl::Contrast, 0.52),
            (CameraControl::Saturation, 0.55),
            (CameraControl::Sharpness, 0.48),
        ],
    },
];

/// One built-in profile: an id for persistence plus range-relative targets
/// (an empty list means "device defaults for everything").
struct BuiltinProfile {
    id: &'static str,
    values: &'static [(CameraControl, f32)],
}

pub struct CameraControlsPanel {
    /// Persistence key (`camera:vid:pid:serial:…` or legacy `camera-<uid>`).
    key: Option<String>,
    /// OS capture id used for UVC open/read/write (may change with USB port).
    uid: Option<String>,
    observed: Option<CameraState>,
    applied: Option<OperationStatus>,
    sliders: Vec<ControlSlider>,
    autos: Vec<AutoRow>,
    #[expect(dead_code, reason = "held to keep the AppState subscription alive")]
    state_obs: Subscription,
}

struct ControlSlider {
    control: CameraControl,
    label: SharedString,
    range: ControlRange,
    slider: CommitSlider<i32>,
}

impl ControlSlider {
    /// The control value under the thumb. The slider is this panel's record of
    /// what the hardware holds, so every row and profile reads it from here.
    fn value(&self, cx: &App) -> i32 {
        self.slider.value(cx)
    }

    /// The control's bounds in the order a clamp needs them; a UVC driver may
    /// report them reversed.
    fn bounds(&self) -> SliderRange<i32> {
        SliderRange::new(self.range.min, self.range.max)
    }

    /// Put the thumb on a value the hardware has just taken.
    fn seat(&self, value: i32, window: &mut Window, cx: &mut App) {
        self.slider.seat(value, window, cx);
    }
}

/// Live UI state for one device-supported auto mode.
struct AutoRow {
    toggle: AutoToggle,
    on: bool,
    default: bool,
}

impl CameraControlsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::observe_panel(
            cx,
            |event| {
                matches!(
                    event,
                    StateEvent::CameraChanged
                        | StateEvent::CameraPermissionChanged
                        | StateEvent::PeripheralsChanged
                        | StateEvent::LanguageChanged
                )
            },
            |panel: &mut Self, cx| {
                panel.sync_selection(cx);
                for slider in &mut panel.sliders {
                    slider.label = control_label(slider.control);
                }
            },
        );
        let mut panel = Self {
            key: None,
            uid: None,
            observed: None,
            applied: None,
            sliders: Vec::new(),
            autos: Vec::new(),
            state_obs,
        };
        panel.sync_selection(cx);
        panel
    }

    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((key, uid)) = Self::active_camera(cx) {
            self.ensure_built(&key, &uid, cx);
        } else {
            self.key = None;
            self.uid = None;
            self.sliders.clear();
            self.autos.clear();
            self.observed = None;
            self.applied = None;
        }
    }

    /// The active camera's `(config_key, capture_id)`, if a webcam is selected.
    fn active_camera(cx: &Context<Self>) -> Option<(String, String)> {
        let record = AppState::try_read(cx)?.current_record()?;
        if !matches!(record.kind, openlogi_core::device::DeviceKind::Camera) {
            return None;
        }
        Some((record.config_key.clone(), record.capture_id.clone()?))
    }

    /// Rebuild from the agent's measured state after an operation completes.
    fn ensure_built(&mut self, key: &str, uid: &str, cx: &mut Context<Self>) {
        let record = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .and_then(|r| r.peripheral.as_ref());
        let observed = record.and_then(|record| {
            record
                .capabilities
                .iter()
                .find_map(|capability| match &capability.capability {
                    Capability::Camera(camera) => camera.state.clone(),
                    _ => None,
                })
        });
        let status = record.and_then(|record| record.operations.first()).cloned();
        if self.key.as_deref() == Some(key) && self.uid.as_deref() == Some(uid) {
            if self.observed == observed && self.applied == status {
                return;
            }
            if status
                .as_ref()
                .is_some_and(|s| s.application == ApplicationStatus::Pending)
                && !self.sliders.is_empty()
            {
                self.applied = status;
                return;
            }
        }
        self.key = Some(key.into());
        self.uid = Some(uid.into());
        self.observed.clone_from(&observed);
        self.applied = status;
        self.sliders.clear();
        self.autos.clear();
        let Some(observed) = observed else {
            return;
        };
        self.autos = observed
            .autos
            .into_iter()
            .map(|(toggle, state)| AutoRow {
                toggle,
                on: state.current,
                default: state.default,
            })
            .collect();
        for (control, range) in observed.controls {
            self.push_control_slider(control, range, range.current, uid, key, cx);
        }
    }

    /// Build one control's slider (seeded to `shown`), wire its release-writes
    /// to the device, and push it onto the panel.
    fn push_control_slider(
        &mut self,
        control: CameraControl,
        range: ControlRange,
        shown: i32,
        uid: &str,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        let uid_for_event = uid.to_string();
        let key_for_event = key.to_string();
        // A drag updates the label; the USB write lands once on release so we
        // don't flood the camera with intermediate values. UVC ranges can be
        // entirely negative (exposure reports e.g. -11..-2), which
        // `SliderRange` builds in the order `SliderState` tolerates.
        let slider = CommitSlider::new(
            SliderRange::new(range.min, range.max),
            shown,
            cx,
            move |panel: &mut Self, v, cx| {
                panel.commit_release(control, &uid_for_event, &key_for_event, v, cx);
            },
        );
        self.sliders.push(ControlSlider {
            control,
            label: control_label(control),
            range,
            slider,
        });
    }

    /// Save one release and its manual-mode takeover in one configuration revision.
    fn commit_release(
        &mut self,
        control: CameraControl,
        uid: &str,
        key: &str,
        value: i32,
        cx: &mut Context<Self>,
    ) {
        if self.uid.as_deref() != Some(uid) {
            return;
        }
        let mut autos = Vec::new();
        if let Some(toggle) = control.auto_toggle()
            && let Some(row) = self
                .autos
                .iter_mut()
                .find(|row| row.toggle == toggle && row.on)
        {
            row.on = false;
            autos.push((toggle, false));
        }
        AppState::apply(cx, |state| {
            state.commit_camera_settings(key, &autos, &[(control, value)])
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// The current auto state gating `control`, if the device has that toggle.
    fn auto_state_for(&self, control: CameraControl) -> Option<bool> {
        let toggle = control.auto_toggle()?;
        self.autos.iter().find(|a| a.toggle == toggle).map(|a| a.on)
    }

    /// Flip one auto mode. Turning auto off re-asserts the slider's value so
    /// the hardware ends where the UI shows, in the same device-open.
    fn toggle_auto(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let Some(row) = self.autos.get(ix) else {
            return;
        };
        let toggle = row.toggle;
        let on = !row.on;
        let mut values = Vec::new();
        if !on
            && let Some(slider) = self
                .sliders
                .iter()
                .find(|s| s.control.auto_toggle() == Some(toggle))
        {
            values.push((slider.control, slider.value(cx)));
        }
        self.autos[ix].on = on;
        AppState::apply(cx, |state| {
            state.commit_camera_settings(&key, &[(toggle, on)], &values)
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Reset every control and auto mode to the device defaults, in one
    /// batched device-open. All rows persist together or not at all — a
    /// per-row loop would silently skip the remaining rows once a failure
    /// invalidated the panel, leaving a mix of reset and stale saved values.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let autos: Vec<(AutoToggle, bool)> = self
            .autos
            .iter()
            .map(|row| (row.toggle, row.default))
            .collect();
        let values: Vec<(CameraControl, i32)> = self
            .sliders
            .iter()
            .map(|s| (s.control, s.range.default))
            .collect();
        self.commit_batch(&key, &autos, &values, window, cx);
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Publish desired values while the agent applies and verifies the saved batch.
    fn commit_batch(
        &mut self,
        key: &str,
        autos: &[(AutoToggle, bool)],
        values: &[(CameraControl, i32)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (toggle, on) in autos {
            if let Some(row) = self.autos.iter_mut().find(|a| a.toggle == *toggle) {
                row.on = *on;
            }
        }
        for (control, value) in values {
            if let Some(slider) = self.sliders.iter().find(|s| s.control == *control) {
                slider.seat(*value, window, cx);
            }
        }
        AppState::apply(cx, |state| state.commit_camera_settings(key, autos, values));
    }

    /// Save one control's device defaults, including its paired auto mode.
    fn reset_control(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let Some((control, value)) = self.sliders.get(ix).map(|s| (s.control, s.range.default))
        else {
            return;
        };
        let autos: Vec<_> = control
            .auto_toggle()
            .and_then(|toggle| {
                self.autos
                    .iter()
                    .find(|row| row.toggle == toggle)
                    .map(|row| (toggle, row.default))
            })
            .into_iter()
            .collect();
        self.commit_batch(&key, &autos, &[(control, value)], window, cx);
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Save a profile's desired controls and selection for agent reconciliation.
    fn apply_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let custom = AppState::try_read(cx)
            .map(|s| s.camera_profiles(&key))
            .unwrap_or_default();

        // Auto targets: built-ins engage every auto mode except Default, which
        // restores the device's own default states; customs use their snapshot
        // (falling back to the current state for toggles they don't record).
        let mut autos: Vec<(AutoToggle, bool)> = Vec::new();
        let mut values: Vec<(CameraControl, i32)> = Vec::new();
        if let Some(builtin) = BUILTIN_PROFILES.iter().find(|p| p.id == id) {
            for row in &self.autos {
                autos.push((
                    row.toggle,
                    if builtin.id == "default" {
                        row.default
                    } else {
                        true
                    },
                ));
            }
            for slider in &self.sliders {
                let fallback = if builtin.id != "default"
                    && matches!(
                        slider.control,
                        CameraControl::PowerLineFrequency | CameraControl::LowLightCompensation
                    ) {
                    slider.value(cx)
                } else {
                    slider.range.default
                };
                let target = builtin
                    .values
                    .iter()
                    .find(|(c, _)| *c == slider.control)
                    .map_or(fallback, |(_, pct)| {
                        let span = to_slider(slider.range.max - slider.range.min);
                        slider.range.min + from_slider(span * pct)
                    });
                values.push((slider.control, slider.bounds().clamp(target)));
            }
        } else if let Some(snap) = custom.get(id) {
            for row in &self.autos {
                let on = snap.0.get(row.toggle.name()).map_or(row.on, |v| *v != 0);
                autos.push((row.toggle, on));
            }
            for slider in &self.sliders {
                if let Some(v) = snap.0.get(slider.control.name()) {
                    values.push((slider.control, slider.bounds().clamp(*v)));
                }
            }
        } else {
            return;
        }

        self.commit_batch(&key, &autos, &values, window, cx);
        AppState::apply(cx, |state| {
            state.commit_camera_active_profile(&key, Some(id.to_string()))
        });
        cx.notify();
    }

    /// The current control values + auto states as a profile snapshot.
    fn snapshot(&self, cx: &Context<Self>) -> CameraControls {
        let mut snap = CameraControls::default();
        for slider in &self.sliders {
            snap.0
                .insert(slider.control.name().to_string(), slider.value(cx));
        }
        for row in &self.autos {
            snap.0
                .insert(row.toggle.name().to_string(), i32::from(row.on));
        }
        snap
    }

    /// Keep the active *custom* profile tracking live edits: any slider or
    /// auto change writes back into its snapshot, so a profile is always what
    /// you last saw while it was selected. Built-ins are never edited.
    fn sync_active_custom(&self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let snap = self.snapshot(cx);
        AppState::apply(cx, |state| state.sync_active_camera_profile(&key, snap));
    }

    /// Save the current control values + auto states as a new custom profile
    /// (auto-named `Custom N`) and mark it active.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let snap = self.snapshot(cx);
        AppState::apply(cx, |state| {
            let existing = state.camera_profiles(&key);
            let mut n = existing.len() + 1;
            let mut name =
                tr!("actions.custom_profile_number", number => n.to_string()).to_string();
            while existing.contains_key(&name) {
                n += 1;
                name = tr!("actions.custom_profile_number", number => n.to_string()).to_string();
            }
            state
                .save_camera_profile(&key, &name, snap)
                .and(state.commit_camera_active_profile(&key, Some(name)))
        });
        cx.notify();
    }

    /// Delete a saved custom profile. The hardware keeps whatever it's set to —
    /// only the snapshot (and, if it named this profile, the selection) goes.
    fn delete_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        AppState::apply(cx, |state| state.delete_camera_profile(&key, name));
        cx.notify();
    }
}

impl Render for CameraControlsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(key) = &self.key else {
            return div();
        };

        let lens: Vec<usize> = section_indices(&self.sliders, true);
        let image: Vec<usize> = section_indices(&self.sliders, false);

        let mut panel = v_flex().gap_2().w_full();
        if let Some(status) = &self.applied {
            panel = panel.child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(super::super::peripheral::operation_label(status)),
            );
        }
        if let Some(record) = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .and_then(|r| r.peripheral.as_ref())
        {
            if record.physical.is_none() {
                panel = panel.child(
                    div()
                        .text_body()
                        .text_color(pal.text_muted)
                        .child(tr!("peripheral.model_scope")),
                );
            }
            if record.driver_error.is_some()
                || self
                    .applied
                    .as_ref()
                    .is_some_and(|s| matches!(s.application, ApplicationStatus::Failed(_)))
            {
                let session = record.session.clone();
                panel = panel.child(
                    crate::ui::components::control_button("camera-retry")
                        .label(tr!("peripheral.retry"))
                        .disabled(
                            record.connection != ConnectionStatus::Online
                                || AppState::try_read(cx).is_some_and(AppState::peripheral_busy),
                        )
                        .on_click(move |_, _, cx| {
                            AppState::apply(cx, |state| {
                                state.manage_peripheral(
                                    crate::services::ipc::PeripheralOperation::Retry(
                                        session.clone(),
                                    ),
                                )
                            });
                        }),
                );
            }
        }
        if self.sliders.is_empty() {
            return panel.child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(tr!("camera.camera_controls_unavailable")),
            );
        }
        panel = panel.child(profiles_row(key, cx));
        if !lens.is_empty() && !image.is_empty() {
            panel = panel.child(section_label(tr!("camera.lens"), pal).mt_1());
        }
        for ix in lens {
            panel = panel.child(control_row(self, ix, cx));
        }
        if !image.is_empty() && self.sliders.len() != image.len() {
            panel = panel.child(section_label(tr!("camera.image"), pal).mt_1());
        }
        for ix in image {
            panel = panel.child(control_row(self, ix, cx));
        }
        panel.child(reset_button(cx))
    }
}

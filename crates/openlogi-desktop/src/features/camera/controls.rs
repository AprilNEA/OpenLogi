//! Camera controls and profiles.
//!
//! Each slider drives a UVC control straight on the device, so a change is
//! seen by every app that opens the camera — Google Meet, Zoom, OBS — not just
//! our preview. Values are persisted per-camera and re-applied over USB when
//! the camera is next viewed, since the hardware only holds them until it
//! loses power. Focus/exposure/white-balance carry an Auto chip mirroring the
//! device's auto modes; their sliders disable while auto owns the value.
//!
//! Profiles are one-click control snapshots: three built-ins (Default /
//! Streaming / Video call) plus user-saved customs, applied to the hardware in
//! a single batched device-open.

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    Selectable as _, h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_camera::{AutoToggle, CameraControl, CameraState, ControlRange};
use openlogi_core::config::CameraControls;
use tracing::debug;

use crate::state::{AppState, StateEvent};
use crate::ui::components::ProfileTab;
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

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

fn update_camera(cx: &mut App, update: impl FnOnce(&mut AppState)) {
    AppState::update(cx, |state, cx| {
        update(state);
        cx.emit(StateEvent::CameraChanged);
    });
}

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
    sliders: Vec<ControlSlider>,
    autos: Vec<AutoRow>,
    #[expect(dead_code, reason = "held to keep the AppState subscription alive")]
    state_obs: Subscription,
}

struct ControlSlider {
    control: CameraControl,
    label: SharedString,
    range: ControlRange,
    state: Entity<SliderState>,
    #[expect(dead_code, reason = "held to keep the slider subscription alive")]
    sub: Subscription,
}

/// Live UI state for one device-supported auto mode.
struct AutoRow {
    toggle: AutoToggle,
    on: bool,
    default: bool,
}

/// What [`CameraControlsPanel::ensure_built`] should build the panel from after
/// re-asserting saved settings on the hardware.
enum Reapplied {
    /// Nothing needed writing, or the batch stuck — build from the desired
    /// (saved-over-snapshot) state.
    Clean,
    /// The batch failed; build rows from this freshly-read live state.
    Live(CameraState),
    /// The batch failed and the confirming re-read failed too — the true
    /// hardware state is unknown, so the caller must not cache a build.
    Unknown,
}

impl CameraControlsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(
            &AppState::global(cx),
            |_panel, _, event: &StateEvent, cx| {
                if matches!(
                    event,
                    StateEvent::InventoryChanged
                        | StateEvent::DeviceSelected(_)
                        | StateEvent::CameraChanged
                        | StateEvent::CameraPermissionChanged
                ) {
                    cx.notify();
                }
            },
        );
        Self {
            key: None,
            uid: None,
            sliders: Vec::new(),
            autos: Vec::new(),
            state_obs,
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

    /// Re-assert the saved auto/value differences on the hardware in one
    /// device-open, reporting what the caller should build the panel from.
    ///
    /// `apply_settings` isn't atomic — it writes the auto mode, then the value —
    /// so a rejected batch can leave the hardware between states, making the
    /// pre-write snapshot untrustworthy. On failure we clear the active profile
    /// (so a later edit's [`Self::sync_active_custom`] can't overwrite the saved
    /// profile with fallback values) and re-read the device: [`Reapplied::Live`]
    /// carries that truth for the caller to cache, while a re-read that also
    /// fails yields [`Reapplied::Unknown`] — never the stale pre-write state.
    fn reapply_saved(
        key: &str,
        uid: &str,
        apply_autos: &[(AutoToggle, bool)],
        apply_values: &[(CameraControl, i32)],
        cx: &mut Context<Self>,
    ) -> Reapplied {
        if apply_autos.is_empty() && apply_values.is_empty() {
            return Reapplied::Clean;
        }
        let Err(e) = openlogi_camera::apply_settings(uid, apply_autos, apply_values) else {
            return Reapplied::Clean;
        };
        debug!(error = %e, "saved camera state reapply failed");
        // This runs while building the panel. Do not emit back into this same
        // view: if the confirming read also fails, an event-driven repaint
        // would immediately retry forever instead of waiting for a real UI or
        // inventory event.
        AppState::update(cx, |state, _| {
            state.set_camera_active_profile(key, None);
        });
        match openlogi_camera::read_camera_state(uid) {
            Ok(live) => Reapplied::Live(live),
            Err(e) => {
                debug!(error = %e, "post-failure camera re-read failed");
                Reapplied::Unknown
            }
        }
    }

    /// Build the sliders and auto rows for `key` from the device's reported
    /// state, re-applying any saved values in one batched device write. Cheap
    /// no-op when already built for this camera. `uid` is the OS capture id.
    fn ensure_built(&mut self, key: &str, uid: &str, cx: &mut Context<Self>) {
        if self.key.as_deref() == Some(key) && self.uid.as_deref() == Some(uid) {
            return;
        }
        self.sliders.clear();
        self.autos.clear();
        // Port-bound keys from older builds → stable serial key, once per open.
        // This is part of render-time panel construction; emitting an event
        // here would create a hot repaint loop while an unavailable camera
        // keeps failing the state read below.
        AppState::update(cx, |state, _| {
            state.migrate_legacy_camera_key(key, uid);
        });

        // One device-open reads every control and auto state. A failed read
        // means the camera is unreachable (unplugged or seized by another app):
        // leave `self.key` unset so the next render retries, instead of caching
        // an empty panel that never rebuilds once the device returns.
        let Ok(snap) = openlogi_camera::read_camera_state(uid) else {
            debug!("camera state read failed; retrying next render");
            self.key = None;
            self.uid = None;
            return;
        };
        self.key = Some(key.to_string());
        self.uid = Some(uid.to_string());

        // Saved auto states win over the device's, then saved values win for
        // controls whose auto is off; the differences push back in one open.
        let mut desired_autos = Vec::new();
        let mut apply_autos = Vec::new();
        for (toggle, st) in &snap.autos {
            let saved = AppState::try_read(cx).and_then(|s| s.camera_auto(key, *toggle));
            let on = saved.unwrap_or(st.current);
            if on != st.current {
                apply_autos.push((*toggle, on));
            }
            desired_autos.push((*toggle, on, *st));
        }
        let auto_desired = |control: CameraControl| {
            let toggle = control.auto_toggle()?;
            desired_autos
                .iter()
                .find(|(t, ..)| *t == toggle)
                .map(|(_, on, _)| *on)
        };
        let mut desired_values = Vec::new();
        let mut apply_values = Vec::new();
        for (control, range) in &snap.controls {
            let saved = AppState::try_read(cx).and_then(|s| s.camera_control(key, *control));
            let initial = saved.unwrap_or(range.current).clamp(range.min, range.max);
            if saved.is_some()
                && saved != Some(range.current)
                && !auto_desired(*control).is_some_and(|on| on)
            {
                apply_values.push((*control, initial));
            }
            desired_values.push((*control, *range, initial));
        }

        // Saved state only sticks when the hardware takes it. On a rejected
        // (non-atomic) batch, rebuild rows from the device's live state; if even
        // that read fails, the hardware state is unknown — drop the key and let
        // the next render retry rather than caching the stale pre-write values.
        let live = match Self::reapply_saved(key, uid, &apply_autos, &apply_values, cx) {
            Reapplied::Clean => None,
            Reapplied::Live(state) => Some(state),
            Reapplied::Unknown => {
                self.key = None;
                self.uid = None;
                return;
            }
        };

        for (toggle, on, st) in desired_autos {
            let shown_on = match &live {
                None => on,
                Some(state) => state
                    .autos
                    .iter()
                    .find(|(t, _)| *t == toggle)
                    .map_or(st.current, |(_, s)| s.current),
            };
            self.autos.push(AutoRow {
                toggle,
                on: shown_on,
                default: st.default,
            });
        }
        for (control, range, initial) in desired_values {
            let shown = match &live {
                None => initial,
                Some(state) => state
                    .controls
                    .iter()
                    .find(|(c, _)| *c == control)
                    .map_or(range.current, |(_, r)| r.current),
            };
            self.push_control_slider(control, range, shown, uid, key, cx);
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
        let state = cx.new(|_| {
            let (lo, hi) = (to_slider(range.min), to_slider(range.max));
            // `SliderState` defaults to [0, 100] and re-clamps its value on every
            // builder call, panicking if min > max even transiently. A fully
            // negative range (UVC exposure reports e.g. -11..-2) would make
            // `.max(-2)` clamp against the default min of 0 — so set the min
            // first for negative ranges, and the max first otherwise.
            let bounded = if lo < 0.0 {
                SliderState::new().min(lo).max(hi)
            } else {
                SliderState::new().max(hi).min(lo)
            };
            bounded.step(1.0).default_value(to_slider(shown))
        });
        let uid_for_event = uid.to_string();
        let key_for_event = key.to_string();
        let sub = cx.subscribe(&state, move |panel, _slider, event: &SliderEvent, cx| {
            match event {
                // Drag updates the label; the USB write lands once on release
                // so we don't flood the camera with intermediate values.
                SliderEvent::Change(_) => cx.notify(),
                SliderEvent::Release(value) => {
                    let v = from_slider(value.start());
                    panel.commit_release(control, &uid_for_event, &key_for_event, v, cx);
                }
            }
        });
        self.sliders.push(ControlSlider {
            control,
            label: control_label(control),
            range,
            state,
            sub,
        });
    }

    /// One slider release: write the value — taking the control over to manual
    /// first when its auto mode owns it (the camera rejects gated values, and
    /// grabbing the slider *is* the take-over gesture, as in G HUB) — then
    /// persist exactly what the device took.
    fn commit_release(
        &mut self,
        control: CameraControl,
        uid: &str,
        key: &str,
        v: i32,
        cx: &mut Context<Self>,
    ) {
        let takeover = control.auto_toggle().and_then(|toggle| {
            let ix = self.autos.iter().position(|a| a.toggle == toggle && a.on)?;
            Some((toggle, ix))
        });
        let written = match takeover {
            Some((toggle, _)) => {
                openlogi_camera::apply_settings(uid, &[(toggle, false)], &[(control, v)])
            }
            None => openlogi_camera::set_control(uid, control, v),
        };
        if let Err(e) = written {
            debug!(?control, value = v, error = %e, "camera control write failed");
            // The slider already moved to `v` on release, but the camera kept its
            // old register (a plain write is atomic; a takeover can land auto-off
            // before the value fails). Rebuild from live hardware so the panel
            // never shows a value the device didn't take.
            self.resync_after_failed_write(cx);
            return;
        }
        if let Some((toggle, ix)) = takeover {
            self.autos[ix].on = false;
            update_camera(cx, |state| {
                state.commit_camera_auto(key, toggle, false);
            });
        }
        update_camera(cx, |state| {
            state.commit_camera_control(key, control, v);
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
        let (Some(key), Some(uid)) = (self.key.clone(), self.uid.clone()) else {
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
            values.push((
                slider.control,
                from_slider(slider.state.read(cx).value().start()),
            ));
        }
        if let Err(e) = openlogi_camera::apply_settings(&uid, &[(toggle, on)], &values) {
            debug!(?toggle, on, error = %e, "camera auto write failed");
            // Turning auto off batches the slider value, so a partial write can
            // land the mode but not the value; resync from live hardware.
            self.resync_after_failed_write(cx);
            return;
        }
        self.autos[ix].on = on;
        update_camera(cx, |state| {
            state.commit_camera_auto(&key, toggle, on);
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Reset every control and auto mode to the device defaults, in one
    /// batched device-open. All rows persist together or not at all — a
    /// per-row loop would silently skip the remaining rows once a failure
    /// invalidated the panel, leaving a mix of reset and stale saved values.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(uid)) = (self.key.clone(), self.uid.clone()) else {
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
        if let Err(e) = openlogi_camera::apply_settings(&uid, &autos, &values) {
            debug!(error = %e, "camera reset failed");
            // Partial writes may have landed; rebuild from live hardware
            // rather than persisting a mixed reset.
            self.resync_after_failed_write(cx);
            return;
        }
        self.commit_batch(&key, &autos, &values, window, cx);
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// After a successful batched write: mirror `autos` + `values` onto the
    /// rows, re-seat the sliders, and persist everything to the config.
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
                slider.state.clone().update(cx, |s, cx| {
                    s.set_value(to_slider(*value), window, cx);
                });
            }
        }
        update_camera(cx, |state| {
            for (toggle, on) in autos {
                state.commit_camera_auto(key, *toggle, *on);
            }
            for (control, value) in values {
                state.commit_camera_control(key, *control, *value);
            }
        });
    }

    /// Reset one control to its device default — auto mode back to the
    /// device's default state, the value re-seated and persisted.
    fn reset_control(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(uid)) = (self.key.clone(), self.uid.clone()) else {
            return;
        };
        let Some((control, default, state)) = self
            .sliders
            .get(ix)
            .map(|s| (s.control, s.range.default, s.state.clone()))
        else {
            return;
        };
        let mut autos = Vec::new();
        let auto_pos = control.auto_toggle().and_then(|toggle| {
            let pos = self.autos.iter().position(|a| a.toggle == toggle)?;
            autos.push((toggle, self.autos[pos].default));
            Some(pos)
        });
        if let Err(e) = openlogi_camera::apply_settings(&uid, &autos, &[(control, default)]) {
            debug!(?control, value = default, error = %e, "camera control reset failed");
            // Auto default + value default aren't atomic; resync from live
            // hardware so a partial reset can't desync the row.
            self.resync_after_failed_write(cx);
            return;
        }
        if let Some(pos) = auto_pos {
            let (toggle, auto_default) = autos[0];
            self.autos[pos].on = auto_default;
            update_camera(cx, |state| {
                state.commit_camera_auto(&key, toggle, auto_default);
            });
        }
        state.update(cx, |slider, cx| {
            slider.set_value(to_slider(default), window, cx);
        });
        update_camera(cx, |state| {
            state.commit_camera_control(&key, control, default);
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Apply a built-in or saved profile: compute each control's target, push
    /// everything to the hardware in one batched open, re-seat the sliders,
    /// persist the values, and remember the selection.
    fn apply_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(uid)) = (self.key.clone(), self.uid.clone()) else {
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
                    from_slider(slider.state.read(cx).value().start())
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
                values.push((
                    slider.control,
                    target.clamp(slider.range.min, slider.range.max),
                ));
            }
        } else if let Some(snap) = custom.get(id) {
            for row in &self.autos {
                let on = snap.0.get(row.toggle.name()).map_or(row.on, |v| *v != 0);
                autos.push((row.toggle, on));
            }
            for slider in &self.sliders {
                if let Some(v) = snap.0.get(slider.control.name()) {
                    values.push((
                        slider.control,
                        (*v).clamp(slider.range.min, slider.range.max),
                    ));
                }
            }
        } else {
            return;
        }

        if let Err(e) = openlogi_camera::apply_settings(&uid, &autos, &values) {
            debug!(profile = id, error = %e, "camera profile apply failed");
            // Some writes may have landed; resync from live state and drop the
            // active profile so a later edit can't persist a half-applied one.
            self.resync_after_failed_write(cx);
            return;
        }
        self.commit_batch(&key, &autos, &values, window, cx);
        update_camera(cx, |state| {
            state.set_camera_active_profile(&key, Some(id.to_string()));
        });
        cx.notify();
    }

    /// The current control values + auto states as a profile snapshot.
    fn snapshot(&self, cx: &Context<Self>) -> CameraControls {
        let mut snap = CameraControls::default();
        for slider in &self.sliders {
            snap.0.insert(
                slider.control.name().to_string(),
                from_slider(slider.state.read(cx).value().start()),
            );
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
        update_camera(cx, |state| {
            let Some(active) = state.camera_active_profile(&key) else {
                return;
            };
            if state.camera_profiles(&key).contains_key(&active) {
                state.save_camera_profile(&key, &active, snap);
            }
        });
    }

    /// Recover after a batched device write failed partway through.
    /// `apply_settings` is not atomic (it writes the auto mode, then the value,
    /// in one open), so a partial failure can leave the hardware between the old
    /// and new state. Drop the cached rows so the panel rebuilds from the
    /// device's live state on the next render, and clear any active profile so a
    /// later edit's [`Self::sync_active_custom`] can't overwrite a saved profile
    /// with those rebuilt values.
    fn resync_after_failed_write(&mut self, cx: &mut Context<Self>) {
        self.uid = None;
        if let Some(key) = self.key.take() {
            update_camera(cx, |state| {
                state.set_camera_active_profile(&key, None);
            });
        }
        cx.notify();
    }

    /// Save the current control values + auto states as a new custom profile
    /// (auto-named `Custom N`) and mark it active.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let snap = self.snapshot(cx);
        update_camera(cx, |state| {
            let existing = state.camera_profiles(&key);
            let mut n = existing.len() + 1;
            let mut name = format!("Custom {n}");
            while existing.contains_key(&name) {
                n += 1;
                name = format!("Custom {n}");
            }
            state.save_camera_profile(&key, &name, snap);
            state.set_camera_active_profile(&key, Some(name));
        });
        cx.notify();
    }

    /// Delete a saved custom profile. The hardware keeps whatever it's set to —
    /// only the snapshot (and, if it named this profile, the selection) goes.
    fn delete_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        update_camera(cx, |state| {
            state.delete_camera_profile(&key, name);
        });
        cx.notify();
    }
}

impl Render for CameraControlsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some((key, uid)) = Self::active_camera(cx) else {
            self.key = None;
            self.uid = None;
            self.sliders.clear();
            self.autos.clear();
            return div().into_any_element();
        };
        self.ensure_built(&key, &uid, cx);

        if self.sliders.is_empty() {
            return div()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("This camera exposes no adjustable image controls."))
                .into_any_element();
        }

        let lens: Vec<usize> = section_indices(&self.sliders, true);
        let image: Vec<usize> = section_indices(&self.sliders, false);

        let mut panel = v_flex().gap_2().w_full().child(profiles_row(&key, cx));
        if !lens.is_empty() && !image.is_empty() {
            panel = panel.child(section_label(tr!("Lens"), pal).mt_1());
        }
        for ix in lens {
            panel = panel.child(control_row(self, ix, cx, pal));
        }
        if !image.is_empty() && self.sliders.len() != image.len() {
            panel = panel.child(section_label(tr!("Image"), pal).mt_1());
        }
        for ix in image {
            panel = panel.child(control_row(self, ix, cx, pal));
        }
        panel.child(reset_button(pal, cx)).into_any_element()
    }
}

/// Indices of the lens (camera-terminal) or image (processing-unit) sliders,
/// preserving [`CameraControl::ALL`] order.
fn section_indices(sliders: &[ControlSlider], lens: bool) -> Vec<usize> {
    sliders
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.control,
                CameraControl::Zoom | CameraControl::Focus | CameraControl::Exposure
            ) == lens
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// The one-click profile chips: built-ins, saved customs, then Save.
fn profiles_row(key: &str, cx: &mut Context<CameraControlsPanel>) -> AnyElement {
    let state = AppState::try_read(cx);
    let active = state.and_then(|s| s.camera_active_profile(key));
    let customs: Vec<String> = state
        .map(|s| s.camera_profiles(key).keys().cloned().collect())
        .unwrap_or_default();

    let mut row = h_flex().flex_wrap().gap_1p5().items_center();
    for (ix, builtin) in BUILTIN_PROFILES.iter().enumerate() {
        let id = builtin.id;
        row = row.child(
            ProfileTab::new(("camera-profile-builtin", ix), builtin_label(id))
                .selected(active.as_deref() == Some(id))
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.apply_profile(id, window, cx);
                })),
        );
    }
    for (ix, name) in customs.into_iter().enumerate() {
        let is_active = active.as_deref() == Some(name.as_str());
        let apply_name = name.clone();
        let delete_name = name.clone();
        let on_apply = cx.listener(move |panel, _: &ClickEvent, window, cx| {
            panel.apply_profile(&apply_name, window, cx);
        });
        let on_delete = cx.listener(move |panel, _: &ClickEvent, _window, cx| {
            panel.delete_profile(&delete_name, cx);
        });
        row = row.child(
            ProfileTab::new(("camera-profile-custom", ix), name)
                .selected(is_active)
                .on_click(on_apply)
                .on_delete(("camera-profile-del", ix), on_delete),
        );
    }
    row = row.child(
        ProfileTab::new("camera-profile-save", format!("+ {}", tr!("New"))).on_click(cx.listener(
            |panel, _: &ClickEvent, _window, cx| {
                panel.save_profile(cx);
            },
        )),
    );
    row.into_any_element()
}

/// One compact control line: label · slider · live value (· Auto chip when the
/// device pairs one). Double-click anywhere on the line resets that control.
fn control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> AnyElement {
    let slider = &panel.sliders[ix];
    if slider.control == CameraControl::PowerLineFrequency
        && [1, 2, 3]
            .into_iter()
            .any(|value| slider.range.supports(value))
    {
        return frequency_row(panel, ix, cx, pal);
    }
    if slider.control == CameraControl::LowLightCompensation
        && slider.range.min == 0
        && slider.range.max == 1
    {
        return binary_control_row(panel, ix, cx, pal);
    }
    let value = from_slider(slider.state.read(cx).value().start());
    let auto_on = panel.auto_state_for(slider.control);
    let dimmed = auto_on == Some(true);

    let mut row = h_flex()
        .id(("camera-control-row", ix))
        .w_full()
        .gap_3()
        .items_center()
        // Capture phase, so the double-click wins over the slider's own
        // handlers: the thumb's mouse-down stops propagation (a bubbled click
        // never fires), and a track click would jump the value and then
        // re-commit it from its deferred Release event after the reset ran.
        .capture_any_mouse_down(cx.listener(
            move |panel, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left && event.click_count == 2 {
                    cx.stop_propagation();
                    panel.reset_control(ix, window, cx);
                }
            },
        ))
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .truncate()
                .text_body()
                .text_color(pal.text_muted)
                .child(slider.label.clone()),
        )
        .child(
            div()
                .flex_1()
                // Dimmed while auto owns the value, but still draggable —
                // grabbing the slider takes the control over to manual.
                .when(dimmed, |s| s.opacity(0.55))
                .child(Slider::new(&slider.state).horizontal()),
        )
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .text_right()
                .text_body()
                .text_color(if dimmed {
                    pal.text_muted
                } else {
                    rgb(ACCENT_BLUE).into()
                })
                .child(format!("{value}")),
        );

    // Every row carries the trailing Auto column — empty for controls without
    // an auto mode — so the sliders and values align across the whole panel.
    let mut auto_cell = div().w(px(46.)).flex_shrink_0().flex().justify_end();
    if let Some(on) = auto_on
        && let Some(toggle) = slider.control.auto_toggle()
        && let Some(auto_ix) = panel.autos.iter().position(|a| a.toggle == toggle)
    {
        let accent = rgb(ACCENT_BLUE);
        auto_cell = auto_cell.child(
            div()
                .id(("camera-control-auto", ix))
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(if on { accent.into() } else { pal.border })
                .text_caption()
                .text_color(if on { pal.text_primary } else { pal.text_muted })
                .bg(if on {
                    theme::accent_tint()
                } else {
                    pal.control
                })
                .hover(move |s| {
                    s.bg(if on {
                        theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
                .child(tr!("Auto"))
                .on_click(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                    panel.toggle_auto(auto_ix, cx);
                })),
        );
    }
    row = row.child(auto_cell);

    row.into_any_element()
}

fn frequency_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> AnyElement {
    let slider = &panel.sliders[ix];
    let current = from_slider(slider.state.read(cx).value().start());
    let mut choices = h_flex().flex_1().justify_end().gap_1();
    for (choice_ix, (value, label)) in [
        (1, SharedString::from("50 Hz")),
        (2, SharedString::from("60 Hz")),
        (3, tr!("Auto")),
    ]
    .into_iter()
    .filter(|(value, _)| slider.range.supports(*value))
    .enumerate()
    {
        let active = value == current;
        let accent = rgb(ACCENT_BLUE);
        choices = choices.child(
            div()
                .id(("camera-frequency", choice_ix))
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(if active { accent.into() } else { pal.border })
                .text_caption()
                .text_color(if active {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .bg(if active {
                    theme::accent_tint()
                } else {
                    pal.control
                })
                .hover(move |s| {
                    s.bg(if active {
                        theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
                .child(label)
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    let (Some(key), Some(uid)) = (panel.key.clone(), panel.uid.clone()) else {
                        return;
                    };
                    panel.sliders[ix].state.clone().update(cx, |state, cx| {
                        state.set_value(to_slider(value), window, cx);
                    });
                    panel.commit_release(CameraControl::PowerLineFrequency, &uid, &key, value, cx);
                })),
        );
    }

    h_flex()
        .id(("camera-control-row", ix))
        .w_full()
        .gap_3()
        .items_center()
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .truncate()
                .text_body()
                .text_color(pal.text_muted)
                .child(slider.label.clone()),
        )
        .child(choices)
        .into_any_element()
}

fn binary_control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> AnyElement {
    let slider = &panel.sliders[ix];
    let on = from_slider(slider.state.read(cx).value().start()) != 0;
    let accent = rgb(ACCENT_BLUE);
    h_flex()
        .id(("camera-control-row", ix))
        .w_full()
        .gap_3()
        .items_center()
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .truncate()
                .text_body()
                .text_color(pal.text_muted)
                .child(slider.label.clone()),
        )
        .child(div().flex_1())
        .child(
            div()
                .id("camera-low-light")
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(if on { accent.into() } else { pal.border })
                .text_caption()
                .text_color(if on { pal.text_primary } else { pal.text_muted })
                .bg(if on {
                    theme::accent_tint()
                } else {
                    pal.control
                })
                .hover(move |s| {
                    s.bg(if on {
                        theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
                .child(if on { tr!("On") } else { tr!("Off") })
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    let (Some(key), Some(uid)) = (panel.key.clone(), panel.uid.clone()) else {
                        return;
                    };
                    let value = i32::from(!on);
                    panel.sliders[ix].state.clone().update(cx, |state, cx| {
                        state.set_value(to_slider(value), window, cx);
                    });
                    panel.commit_release(
                        CameraControl::LowLightCompensation,
                        &uid,
                        &key,
                        value,
                        cx,
                    );
                })),
        )
        .into_any_element()
}

fn reset_button(pal: Palette, cx: &mut Context<CameraControlsPanel>) -> AnyElement {
    h_flex()
        .w_full()
        .justify_end()
        .child(
            div()
                .id("camera-controls-reset")
                .px_2p5()
                .py_0p5()
                .rounded_md()
                .border_1()
                .border_color(pal.border)
                .bg(pal.control)
                .hover(|s| s.bg(pal.control_hover))
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("Reset to defaults"))
                .on_click(cx.listener(|panel, _: &ClickEvent, window, cx| {
                    panel.reset(window, cx);
                })),
        )
        .into_any_element()
}

fn builtin_label(id: &str) -> SharedString {
    match id {
        "streaming" => tr!("Streaming"),
        "video_call" => tr!("Video call"),
        _ => tr!("Default"),
    }
}

fn control_label(control: CameraControl) -> SharedString {
    match control {
        CameraControl::Zoom => tr!("Zoom"),
        CameraControl::Focus => tr!("Focus"),
        CameraControl::Exposure => tr!("Exposure"),
        CameraControl::PowerLineFrequency => tr!("Anti-flicker"),
        CameraControl::LowLightCompensation => tr!("Low light compensation"),
        CameraControl::Brightness => tr!("Brightness"),
        CameraControl::Contrast => tr!("Contrast"),
        CameraControl::Saturation => tr!("Saturation"),
        CameraControl::Sharpness => tr!("Sharpness"),
        CameraControl::WhiteBalance => tr!("White balance"),
        CameraControl::Tint => tr!("Tint"),
    }
}

/// A UVC control value as the GPUI slider wants it.
#[expect(
    clippy::cast_precision_loss,
    reason = "a UVC control range is far below f32's exact integer range"
)]
fn to_slider(value: i32) -> f32 {
    value as f32
}

/// Inverse of [`to_slider`].
#[expect(
    clippy::cast_possible_truncation,
    reason = "the slider steps by 1 over the control's own i32 range"
)]
fn from_slider(value: f32) -> i32 {
    value.round() as i32
}

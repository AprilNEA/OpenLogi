//! Webcam control state and camera profiles.

use openlogi_camera::CameraControl;

use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    /// Request Camera access and retain the permission poll for the app
    /// entity's lifetime. Repeated requests reuse an active poll; a completed
    /// poll may be replaced if authorization is still undetermined.
    #[cfg(target_os = "macos")]
    pub(crate) fn request_camera_access(cx: &mut gpui::App) {
        use std::time::Duration;

        const TICK: Duration = Duration::from_millis(250);
        const TICKS_MAX: u32 = 2400; // 10 minutes

        openlogi_camera::request_camera_access();
        Self::update(cx, |state, cx| {
            if state
                .camera_permission_poll
                .as_ref()
                .is_some_and(|poll| !poll.is_ready())
            {
                return;
            }
            state.camera_permission_poll = Some(cx.spawn(async move |state, cx| {
                for _ in 0..TICKS_MAX {
                    cx.background_executor().timer(TICK).await;
                    if openlogi_camera::camera_authorization()
                        != openlogi_camera::CameraAuthorization::Undetermined
                    {
                        break;
                    }
                }
                state
                    .update(cx, |_, cx| cx.emit(StateEvent::CameraPermissionChanged))
                    .ok();
            }));
        });
    }

    /// Whether any connected device is a webcam. Gates the camera-permission UI
    /// so it only appears when there is actually a camera to grant access to.
    /// Only the platforms that register the permission page (macOS/Linux) call
    /// this; Windows has no such page, so the method is scoped to match.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[must_use]
    pub fn has_camera(&self) -> bool {
        self.devices
            .records
            .iter()
            .any(|r| matches!(r.kind, openlogi_core::device::DeviceKind::Camera))
    }
    /// The saved value of a UVC control for `config_key`, if any.
    #[must_use]
    pub fn camera_control(&self, config_key: &str, control: CameraControl) -> Option<i32> {
        self.config
            .camera_controls(config_key)?
            .0
            .get(control.name())
            .copied()
    }
    /// The saved state of a camera auto toggle for `config_key`, if any.
    #[must_use]
    pub fn camera_auto(
        &self,
        config_key: &str,
        toggle: openlogi_camera::AutoToggle,
    ) -> Option<bool> {
        self.config
            .camera_controls(config_key)?
            .0
            .get(toggle.name())
            .map(|v| *v != 0)
    }
    /// Persist a UVC control for `config_key`. No agent IPC — webcams are
    /// driven straight from the GUI over USB, so the agent never sees this.
    pub fn commit_camera_control(
        &mut self,
        config_key: &str,
        control: CameraControl,
        value: i32,
    ) -> StateEvents {
        self.commit_camera_entry(config_key, control.name(), value);
        StateEvent::CameraChanged.into()
    }
    /// Persist a camera auto toggle for `config_key` (stored as 0/1).
    pub fn commit_camera_auto(
        &mut self,
        config_key: &str,
        toggle: openlogi_camera::AutoToggle,
        on: bool,
    ) -> StateEvents {
        self.commit_camera_entry(config_key, toggle.name(), i32::from(on));
        StateEvent::CameraChanged.into()
    }
    /// Persist a batch of auto toggles and control values for `config_key` as
    /// one announced change — what a reset or an applied profile writes.
    pub fn commit_camera_settings(
        &mut self,
        config_key: &str,
        autos: &[(openlogi_camera::AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> StateEvents {
        for (toggle, on) in autos {
            self.commit_camera_entry(config_key, toggle.name(), i32::from(*on));
        }
        for (control, value) in values {
            self.commit_camera_entry(config_key, control.name(), *value);
        }
        StateEvent::CameraChanged.into()
    }
    fn commit_camera_entry(&mut self, config_key: &str, name: &str, value: i32) {
        let mut controls = self.config.camera_controls(config_key).unwrap_or_default();
        controls.0.insert(name.to_string(), value);
        self.config
            .edit(|config| config.set_camera_controls(config_key, controls));
        self.persist_config("camera controls");
    }
    /// Lift settings from the legacy port-bound `camera-<unique_id>` key onto
    /// the stable serial/model key when the latter has none. Inventory identity
    /// for cameras is separate ([`DeviceRecord::inventory_key`](super::DeviceRecord::inventory_key));
    /// settings never
    /// use capture-id suffixes, so two serial-less same-model units honestly
    /// share one settings bag rather than risk cross-assigning on port moves.
    pub fn migrate_legacy_camera_key(&mut self, config_key: &str, capture_id: &str) {
        if self.camera_key_has_settings(config_key) {
            return;
        }
        let port_key = format!("camera-{capture_id}");
        if port_key == config_key || !self.camera_key_has_settings(&port_key) {
            return;
        }
        let controls = self.config.camera_controls(&port_key);
        let profiles = self.config.camera_profiles(&port_key);
        let active = self.config.camera_active_profile(&port_key);
        self.config.edit(|config| {
            if let Some(controls) = controls {
                config.set_camera_controls(config_key, controls);
            }
            for (name, snap) in profiles {
                config.save_camera_profile(config_key, &name, snap);
            }
            if let Some(active) = active {
                config.set_camera_active_profile(config_key, Some(active));
            }
            config.devices.remove(&port_key);
        });
        self.persist_config("camera key migration");
    }
    fn camera_key_has_settings(&self, key: &str) -> bool {
        self.config.camera_controls(key).is_some()
            || !self.config.camera_profiles(key).is_empty()
            || self.config.camera_active_profile(key).is_some()
    }
    /// User-saved camera profiles for `config_key` (name → snapshot).
    #[must_use]
    pub fn camera_profiles(
        &self,
        config_key: &str,
    ) -> std::collections::BTreeMap<String, openlogi_core::config::CameraControls> {
        self.config.camera_profiles(config_key)
    }
    /// Save a custom camera profile and persist it.
    pub fn save_camera_profile(
        &mut self,
        config_key: &str,
        name: &str,
        snap: openlogi_core::config::CameraControls,
    ) -> StateEvents {
        self.config
            .edit(|config| config.save_camera_profile(config_key, name, snap));
        self.persist_config("camera profile");
        StateEvent::CameraChanged.into()
    }
    /// Write `snap` back into the active profile when it is a saved custom
    /// one, so a profile is always what was last seen while it was selected.
    /// Built-in profiles are never edited.
    pub fn sync_active_camera_profile(
        &mut self,
        config_key: &str,
        snap: openlogi_core::config::CameraControls,
    ) -> StateEvents {
        let Some(active) = self.camera_active_profile(config_key) else {
            return StateEvent::CameraChanged.into();
        };
        if self.camera_profiles(config_key).contains_key(&active) {
            return self.save_camera_profile(config_key, &active, snap);
        }
        StateEvent::CameraChanged.into()
    }
    /// Delete a custom camera profile and persist the removal.
    pub fn delete_camera_profile(&mut self, config_key: &str, name: &str) -> StateEvents {
        self.config
            .edit(|config| config.delete_camera_profile(config_key, name));
        self.persist_config("camera profile removal");
        StateEvent::CameraChanged.into()
    }
    /// The camera profile last applied for `config_key`, if any.
    #[must_use]
    pub fn camera_active_profile(&self, config_key: &str) -> Option<String> {
        self.config.camera_active_profile(config_key)
    }
    /// Record (and persist) which camera profile `config_key` last applied.
    pub fn commit_camera_active_profile(
        &mut self,
        config_key: &str,
        name: Option<String>,
    ) -> StateEvents {
        self.config
            .edit(|config| config.set_camera_active_profile(config_key, name));
        self.persist_config("camera profile selection");
        StateEvent::CameraChanged.into()
    }
}

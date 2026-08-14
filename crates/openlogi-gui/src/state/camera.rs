//! Webcam control state and camera profiles.

use openlogi_camera::CameraControl;

use super::AppState;

impl AppState {
    /// Whether any connected device is a webcam. Gates the camera-permission UI
    /// so it only appears when there is actually a camera to grant access to.
    /// Only the platforms that register the permission page (macOS/Linux) call
    /// this; Windows has no such page, so the method is scoped to match.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[must_use]
    pub fn has_camera(&self) -> bool {
        self.device_list
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
    pub fn commit_camera_control(&mut self, config_key: &str, control: CameraControl, value: i32) {
        self.commit_camera_entry(config_key, control.name(), value);
    }
    /// Persist a camera auto toggle for `config_key` (stored as 0/1).
    pub fn commit_camera_auto(
        &mut self,
        config_key: &str,
        toggle: openlogi_camera::AutoToggle,
        on: bool,
    ) {
        self.commit_camera_entry(config_key, toggle.name(), i32::from(on));
    }
    fn commit_camera_entry(&mut self, config_key: &str, name: &str, value: i32) {
        let mut controls = self.config.camera_controls(config_key).unwrap_or_default();
        controls.0.insert(name.to_string(), value);
        self.config.set_camera_controls(config_key, controls);
        self.persist_config("camera controls");
    }
    /// Lift settings from the legacy port-bound `camera-<unique_id>` key onto
    /// the stable serial/model key when the latter has none. Inventory identity
    /// for cameras is separate ([`DeviceRecord::inventory_key`]); settings never
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
        if let Some(controls) = self.config.camera_controls(&port_key) {
            self.config.set_camera_controls(config_key, controls);
        }
        for (name, snap) in self.config.camera_profiles(&port_key) {
            self.config.save_camera_profile(config_key, &name, snap);
        }
        if let Some(active) = self.config.camera_active_profile(&port_key) {
            self.config
                .set_camera_active_profile(config_key, Some(active));
        }
        self.config.devices.remove(&port_key);
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
    ) {
        self.config.save_camera_profile(config_key, name, snap);
        self.persist_config("camera profile");
    }
    /// Delete a custom camera profile and persist the removal.
    pub fn delete_camera_profile(&mut self, config_key: &str, name: &str) {
        self.config.delete_camera_profile(config_key, name);
        self.persist_config("camera profile removal");
    }
    /// The camera profile last applied for `config_key`, if any.
    #[must_use]
    pub fn camera_active_profile(&self, config_key: &str) -> Option<String> {
        self.config.camera_active_profile(config_key)
    }
    /// Record (and persist) which camera profile `config_key` last applied.
    pub fn set_camera_active_profile(&mut self, config_key: &str, name: Option<String>) {
        self.config.set_camera_active_profile(config_key, name);
        self.persist_config("camera profile selection");
    }
}

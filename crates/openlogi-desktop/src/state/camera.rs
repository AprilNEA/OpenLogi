//! Webcam control state and camera profiles.

use openlogi_camera::CameraControl;

use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    pub(super) fn reconcile_camera_profiles(&mut self) {
        use openlogi_core::peripheral::{ApplicationStatus, Capability, PeripheralError};
        let cameras: Vec<_> = self
            .agent
            .peripherals
            .devices
            .iter()
            .flat_map(|record| {
                record.capabilities.iter().filter_map(|capability| {
                    let Capability::Camera(camera) = &capability.capability else {
                        return None;
                    };
                    let write_failed = record.operations.iter().any(|s| {
                        s.capability == capability.id
                            && matches!(
                                s.application,
                                ApplicationStatus::Failed(
                                    PeripheralError::WriteFailed(_)
                                        | PeripheralError::ReadbackMismatch
                                )
                            )
                    });
                    Some((
                        camera.camera.config_key(),
                        camera.camera.unique_id.clone(),
                        write_failed,
                    ))
                })
            })
            .collect();
        for (key, uid, write_failed) in cameras {
            self.migrate_legacy_camera_key(&key, &uid);
            // A partial batch must not replace a saved profile with the next observed values.
            if write_failed && self.camera_active_profile(&key).is_some() {
                let _ = self.commit_camera_active_profile(&key, None);
            }
        }
    }

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
    /// Save one desired native-control batch and ask the agent to reconcile it.
    pub fn commit_camera_settings(
        &mut self,
        config_key: &str,
        autos: &[(openlogi_camera::AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> StateEvents {
        let mut controls = self.config.camera_controls(config_key).unwrap_or_default();
        for (toggle, on) in autos {
            controls.0.insert(toggle.name().into(), i32::from(*on));
        }
        for (control, value) in values {
            controls.0.insert(control.name().into(), *value);
        }
        self.config
            .edit(|config| config.set_camera_controls(config_key, controls));
        self.persist_and_reload("camera controls");
        StateEvent::CameraChanged.into()
    }
    /// Lift settings from the legacy port-bound `camera-<unique_id>` key onto
    /// the stable serial/model key when the latter has none. Inventory identity
    /// for cameras is separate ([`DeviceRecord::inventory_key`](super::DeviceRecord::inventory_key));
    /// settings never
    /// use capture-id suffixes, so two serial-less same-model units honestly
    /// share one settings bag rather than risk cross-assigning on port moves.
    pub fn migrate_legacy_camera_key(&mut self, config_key: &str, capture_id: &str) {
        let Some(port_key) = self.config.legacy_camera_key(config_key, capture_id) else {
            return;
        };
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
        self.persist_and_reload("camera key migration");
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

//! Per-device RGB keyboard lighting settings.

use openlogi_core::config::Lighting;
use openlogi_core::device_order::PhysicalDeviceKey;
use tracing::debug;

use super::AppState;

impl AppState {
    /// The lighting config for the active device, or the default when none is
    /// stored / no device is selected.
    #[must_use]
    pub fn lighting(&self) -> Lighting {
        self.current_record()
            .and_then(|record| {
                let key = record.persistent_config_key()?;
                self.lighting_for(key, &record.route_key)
            })
            .unwrap_or_default()
    }
    /// The stored lighting config for `key` on `route_key`, or `None` when
    /// unset (or overridden to unset on that link).
    #[must_use]
    pub fn lighting_for(&self, key: &str, route_key: &str) -> Option<Lighting> {
        if PhysicalDeviceKey::is_transient(key)
            || self
                .devices
                .records
                .iter()
                .any(|record| record.config_key == key && !record.is_persistent())
        {
            return None;
        }
        self.config
            .devices
            .get(key)
            .and_then(|device| device.effective_lighting(route_key))
            .cloned()
    }
    /// Persist a new lighting config for the active device and push it to the
    /// hardware (best-effort). No-op when no device is selected.
    pub fn commit_lighting(&mut self, lighting: Lighting) {
        let Some(record) = self.current_record() else {
            debug!("no active device — lighting change ignored");
            return;
        };
        let key = record.persistent_config_key().map(str::to_string);
        let target = record.route.clone();
        if let Some(key) = key {
            self.config
                .edit(|config| config.set_lighting(&key, lighting.clone()));
            // Keep the agent's config copy fresh: it re-applies the saved colour
            // when the keyboard reconnects, and without the reload it would
            // replay whatever was saved the last time something *else* reloaded.
            if !self.persist_and_reload("lighting") {
                return;
            }
        } else {
            debug!("transient device lighting applied without persistence");
        }
        if let Some(route) = target {
            self.send_ipc(crate::services::ipc::Command::SetLighting(route, lighting));
        }
    }
}

//! Per-device RGB keyboard lighting settings.

use openlogi_agent_core::device_order::PhysicalDeviceKey;
use openlogi_core::config::Lighting;
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::AppState;

impl AppState {
    /// The lighting config for the active device, or the default when none is
    /// stored / no device is selected.
    #[must_use]
    pub fn lighting(&self) -> Lighting {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.lighting(key))
            .unwrap_or_default()
    }
    /// The stored lighting config for `key`, or `None` when unset.
    #[must_use]
    pub fn lighting_for(&self, key: &str) -> Option<Lighting> {
        if PhysicalDeviceKey::is_transient(key)
            || self
                .device_list
                .iter()
                .any(|record| record.config_key == key && !record.is_persistent())
        {
            return None;
        }
        self.config.lighting(key)
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
        if let Some(route) = target {
            self.send_ipc(crate::ipc_client::Command::SetLighting(
                route,
                lighting.clone(),
            ));
        }
        let Some(key) = key else {
            debug!("transient device lighting applied without persistence");
            return;
        };
        self.config.set_lighting(&key, lighting);
        // Keep the agent's config copy fresh: it re-applies the saved colour
        // when the keyboard reconnects, and without the reload it would
        // replay whatever was saved the last time something *else* reloaded.
        self.persist_and_reload("lighting");
    }
}

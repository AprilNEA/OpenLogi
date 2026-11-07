//! Keys a keyboard sends something else for, through its HID++ `0x1b05` key
//! customisation (the G515 LS TKL).
//!
//! A per-device setting the agent acts on: it writes the remap when the
//! keyboard connects and when a reload changes it. The app only persists the
//! remap and reloads the agent; the keyboard has no remap to read back.

use openlogi_core::config::KeyRemap;
use openlogi_device_registry::key_remap::key_sources;
use tracing::debug;

use super::events::StateEvents;
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

impl AppState {
    /// Whether OpenLogi knows the active keyboard's keys well enough to
    /// remap them.
    #[must_use]
    pub fn current_key_remap_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.model_info.as_ref())
            .is_some_and(|info| key_sources(info.model_ids).is_some())
    }

    /// The active keyboard's remap; empty when it has none.
    #[must_use]
    pub fn current_key_remap(&self) -> KeyRemap {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.key_remap(key))
            .cloned()
            .unwrap_or_default()
    }

    /// Persist `remap` for the active keyboard and reload the agent, which
    /// writes it to the keyboard. No-op when no device is selected, its keys
    /// are unknown, or it has no persistent key.
    pub fn commit_key_remap(&mut self, remap: KeyRemap) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        if !self.current_key_remap_supported() {
            debug!("active device has no known keys to remap");
            return events;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — key remap ignored");
            return events;
        };
        self.config.edit(|config| config.set_key_remap(&key, remap));
        self.persist_and_reload("key remap");
        events
    }
}

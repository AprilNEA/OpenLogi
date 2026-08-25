//! Actions Ring settings for the selected device.

use openlogi_core::binding::{ActionRingConfig, ActionRingIcon, ActionRingSlot, RingAction};

use super::{AppState, DeviceRecord};

impl AppState {
    /// Actions Ring settings for the active device, including its implicit
    /// default layout when nothing has been persisted yet.
    #[must_use]
    pub fn current_action_ring(&self) -> ActionRingConfig {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.action_ring(key))
            .unwrap_or_default()
    }

    /// Replace or clear one slot in the active device's default ring layout.
    pub fn commit_action_ring_slot(&mut self, slot: ActionRingSlot, action: Option<RingAction>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_slot(&key, slot, action));
        self.persist_and_reload("Actions Ring slot");
    }

    /// Set or restore the action-derived icon for one active-device ring slot.
    pub fn commit_action_ring_icon(&mut self, slot: ActionRingSlot, icon: Option<ActionRingIcon>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_icon(&key, slot, icon));
        self.persist_and_reload("Actions Ring icon");
    }

    /// Enable or disable the active device's Actions Ring.
    pub fn commit_action_ring_enabled(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_enabled(&key, enabled));
        self.persist_and_reload("Actions Ring enabled state");
    }

    /// Enable or disable hover and activation haptics for the active ring.
    pub fn commit_action_ring_haptics(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_haptics(&key, enabled));
        self.persist_and_reload("Actions Ring haptics");
    }
}

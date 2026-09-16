//! Per-device Flow (edge-triggered host switching) settings.

use openlogi_core::config::{FlowConfig, FlowFollow, FlowSide, FlowTriggerMode};
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::AppState;

impl AppState {
    /// The active device's Flow pointer settings (default when unset).
    #[must_use]
    pub fn current_flow(&self) -> FlowConfig {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.devices.get(key))
            .map(|device| device.flow.clone())
            .unwrap_or_default()
    }

    /// The active device's follower setting (`Auto` when unset).
    #[must_use]
    pub fn current_flow_follow(&self) -> FlowFollow {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.devices.get(key))
            .map(|device| device.flow_follow.clone())
            .unwrap_or_default()
    }

    /// Toggle Flow on the active device; the agent re-arms its watcher on the
    /// config reload.
    pub fn set_flow_enabled(&mut self, enabled: bool) {
        self.edit_current_flow("flow enabled", |flow| flow.enabled = enabled);
    }

    /// Map `side` to `host` (or clear it with `None`) on the active device.
    pub fn set_flow_placement(&mut self, side: FlowSide, host: Option<u8>) {
        self.edit_current_flow("flow placement", |flow| flow.placements.set(side, host));
    }

    /// Move the card on `from` to the empty (or displaced) side `to`. A drop
    /// on the occupied `to` swaps the two cards rather than deleting one.
    pub fn move_flow_placement(&mut self, from: FlowSide, to: FlowSide) {
        self.edit_current_flow("flow placement move", |flow| {
            let moved = flow.placements.get(from);
            let displaced = flow.placements.get(to);
            flow.placements.set(to, moved);
            flow.placements.set(from, displaced);
        });
    }

    /// Point `side`'s card at `host`. When another card already targets
    /// `host`, the two cards swap hosts instead of colliding — the
    /// arrangement always keeps one card per host.
    pub fn assign_flow_host(&mut self, side: FlowSide, host: u8) {
        self.edit_current_flow("flow host", move |flow| {
            let displaced = flow
                .placements
                .iter()
                .find(|&(other, occupied)| other != side && occupied == host)
                .map(|(other, _)| other);
            if let Some(other) = displaced {
                flow.placements.set(other, flow.placements.get(side));
            }
            flow.placements.set(side, Some(host));
        });
    }

    /// Set the active device's trigger mode (edge vs Ctrl+edge).
    pub fn set_flow_trigger(&mut self, trigger: FlowTriggerMode) {
        self.edit_current_flow("flow trigger mode", |flow| flow.trigger = trigger);
    }

    /// Set whether the active device follows a Flow pointer.
    pub fn set_flow_follow(&mut self, follow: FlowFollow) {
        let Some(key) = self.current_persistent_key("flow follow") else {
            return;
        };
        if self
            .config
            .devices
            .get(key.as_str())
            .map_or(FlowFollow::Auto, |device| device.flow_follow.clone())
            == follow
        {
            return;
        }
        self.config
            .edit(|config| config.devices.entry(key).or_default().flow_follow = follow);
        self.persist_and_reload("flow follow");
    }

    /// Every device that could act as the Flow pointer for a follower to bind
    /// to explicitly: pointing-kind records with Flow enabled, as
    /// `(config_key, display name)`.
    #[must_use]
    pub fn flow_pointer_candidates(&self) -> Vec<(String, String)> {
        self.devices()
            .iter()
            .filter(|record| {
                matches!(
                    record.kind,
                    openlogi_core::device::DeviceKind::Mouse
                        | openlogi_core::device::DeviceKind::Trackball
                        | openlogi_core::device::DeviceKind::Touchpad
                )
            })
            .filter_map(|record| {
                let key = record.persistent_config_key()?;
                self.config
                    .devices
                    .get(key)
                    .filter(|device| device.flow.enabled)?;
                Some((key.to_string(), record.display_name.clone()))
            })
            .collect()
    }

    /// Edit the active device's [`FlowConfig`], persist, and reload the
    /// agent. No-op without a persistent key or when nothing changes.
    fn edit_current_flow(&mut self, what: &'static str, edit: impl FnOnce(&mut FlowConfig)) {
        let Some(key) = self.current_persistent_key(what) else {
            return;
        };
        let mut next = self
            .config
            .devices
            .get(key.as_str())
            .map(|device| device.flow.clone())
            .unwrap_or_default();
        edit(&mut next);
        if self
            .config
            .devices
            .get(key.as_str())
            .map(|device| device.flow.clone())
            .unwrap_or_default()
            == next
        {
            return;
        }
        self.config
            .edit(|config| config.devices.entry(key).or_default().flow = next);
        self.persist_and_reload(what);
    }

    /// The active device's writable config key, or a debug-logged `None`.
    fn current_persistent_key(&self, what: &str) -> Option<String> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        if key.is_none() {
            debug!(what, "no persistent device key — flow change ignored");
        }
        key
    }
}

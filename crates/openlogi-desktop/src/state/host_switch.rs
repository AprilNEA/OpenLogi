//! Per-mouse Easy-Switch opt-in; the agent owns protocol support and switching.

use openlogi_core::device::DeviceKind;

use super::{AppState, DeviceRecord, StateEvent, StateEvents};

impl AppState {
    fn current_host_switch_source_key(&self) -> Option<&str> {
        self.current_record()
            .filter(|record| record.kind == DeviceKind::Keyboard)
            .and_then(DeviceRecord::persistent_config_key)
    }

    /// Saved mouse targets for the selected persistent keyboard, empty by default.
    /// References remain visible even when their device is absent or unprobed.
    #[must_use]
    pub fn current_host_switch_targets(&self) -> &[String] {
        self.current_host_switch_source_key()
            .and_then(|key| self.config.devices.get(key))
            .map_or(&[], |device| device.host_switch_targets.as_slice())
    }

    /// Persistent pointer mice eligible for the selected persistent keyboard.
    /// Measured or last-known pointer capabilities are required, not inferred
    /// from kind. This does not claim host-switch protocol support; the agent
    /// checks that. Offline records with retained capabilities remain eligible.
    pub fn current_host_switch_candidates(&self) -> impl Iterator<Item = &DeviceRecord> {
        let source_key = self.current_host_switch_source_key();
        self.devices().iter().filter(move |record| {
            source_key.is_some_and(|source_key| {
                record
                    .persistent_config_key()
                    .is_some_and(|target_key| target_key != source_key)
                    && record.kind == DeviceKind::Mouse
                    && record
                        .capabilities
                        .as_ref()
                        .is_some_and(|caps| caps.pointer)
            })
        })
    }

    /// Add or remove one Easy-Switch mouse target, preserving all other settings.
    /// Enabling requires an eligible candidate. A saved target can always be
    /// removed, including after its record or capabilities disappear.
    pub fn commit_host_switch_target(&mut self, target_key: &str, enabled: bool) -> StateEvents {
        let Some(source_key) = self.current_host_switch_source_key() else {
            return StateEvents::none();
        };
        let linked = self
            .current_host_switch_targets()
            .iter()
            .any(|key| key == target_key);
        if linked == enabled
            || (enabled
                && !self
                    .current_host_switch_candidates()
                    .any(|record| record.config_key == target_key))
        {
            return StateEvents::none();
        }
        let source_key = source_key.to_string();
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        self.config.edit(|config| {
            let targets = &mut config
                .devices
                .entry(source_key)
                .or_default()
                .host_switch_targets;
            if enabled {
                targets.push(target_key.to_string());
            } else {
                targets.retain(|key| key != target_key);
            }
        });
        self.persist_and_reload("Easy-Switch target");
        events
    }
}

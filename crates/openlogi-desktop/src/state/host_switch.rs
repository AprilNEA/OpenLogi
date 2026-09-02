//! Easy-Switch follower configuration projected into the device UI.

use openlogi_core::device::DeviceKind;

use super::{AppState, DeviceKey, DeviceRecord};

/// One device that can follow the selected keyboard's host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostSwitchTargetDevice {
    pub(crate) config_key: String,
    pub(crate) display_name: String,
    pub(crate) kind: DeviceKind,
    pub(crate) online: bool,
    pub(crate) selected: bool,
}

/// Result of attempting to change one Easy-Switch follower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostSwitchTargetUpdate {
    /// The request was invalid or already matched the saved configuration.
    Unchanged,
    /// The new follower selection was persisted and sent to the agent.
    Persisted(DeviceKey),
    /// Persistence failed and the in-memory selection was rolled back.
    RolledBack,
}

impl AppState {
    /// Devices eligible to follow the selected Easy-Switch keyboard.
    #[must_use]
    pub(crate) fn host_switch_target_devices(&self) -> Vec<HostSwitchTargetDevice> {
        let Some(keyboard_key) = self.current_host_switch_keyboard_key() else {
            return Vec::new();
        };
        let selected = self
            .config
            .devices
            .get(keyboard_key)
            .map_or(&[][..], |device| device.host_switch_targets.as_slice());

        self.devices()
            .iter()
            .filter(|record| record.persistent_config_key() != Some(keyboard_key))
            .filter(|record| is_compatible_target(record))
            .filter_map(|record| {
                let config_key = record.persistent_config_key()?.to_string();
                Some(HostSwitchTargetDevice {
                    selected: selected.iter().any(|key| key == &config_key),
                    config_key,
                    display_name: record.display_name.clone(),
                    kind: record.kind,
                    online: record.online,
                })
            })
            .collect()
    }

    /// Add or remove one follower and reload the agent when persistence wins.
    pub(crate) fn set_host_switch_target_enabled(
        &mut self,
        target_key: &str,
        enabled: bool,
    ) -> HostSwitchTargetUpdate {
        let Some(keyboard) = self.current_record() else {
            return HostSwitchTargetUpdate::Unchanged;
        };
        let Some(keyboard_key) = keyboard.persistent_config_key().map(str::to_string) else {
            return HostSwitchTargetUpdate::Unchanged;
        };
        let event_key = keyboard.device_key();
        if target_key == keyboard_key.as_str()
            || !keyboard
                .capabilities
                .is_some_and(|caps| caps.host_switch_controls)
            || !self.devices().iter().any(|record| {
                record.persistent_config_key() == Some(target_key) && is_compatible_target(record)
            })
        {
            return HostSwitchTargetUpdate::Unchanged;
        }

        let changed = self.config.edit(|config| {
            let targets = &mut config
                .devices
                .entry(keyboard_key)
                .or_default()
                .host_switch_targets;
            set_target_enabled(targets, target_key, enabled)
        });
        if !changed {
            HostSwitchTargetUpdate::Unchanged
        } else if self.persist_and_reload("Easy-Switch follower") {
            HostSwitchTargetUpdate::Persisted(event_key)
        } else {
            HostSwitchTargetUpdate::RolledBack
        }
    }

    fn current_host_switch_keyboard_key(&self) -> Option<&str> {
        let record = self.current_record()?;
        record
            .capabilities
            .is_some_and(|caps| caps.host_switch_controls)
            .then(|| record.persistent_config_key())
            .flatten()
    }
}

fn is_compatible_target(record: &DeviceRecord) -> bool {
    record.capabilities.is_some_and(|caps| caps.host_switching)
}

fn set_target_enabled(targets: &mut Vec<String>, target_key: &str, enabled: bool) -> bool {
    let contains = targets.iter().any(|key| key == target_key);
    match (enabled, contains) {
        (true, false) => targets.push(target_key.to_string()),
        (false, true) => targets.retain(|key| key != target_key),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use openlogi_core::config::{Config, ConfigFile};
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };

    use super::{
        super::ConfigPersistence, AppState, DeviceRecord, HostSwitchTargetUpdate,
        is_compatible_target, set_target_enabled,
    };
    use crate::services::assets::AssetResolver;

    fn record(kind: DeviceKind, host_switching: bool) -> DeviceRecord {
        DeviceRecord {
            config_key: "unit:test".into(),
            canonical_key: Some("unit:test".into()),
            persistent: true,
            route_key: "direct:046d:test".into(),
            model_key: "test".into(),
            model_name: "Test device".into(),
            display_name: "Test device".into(),
            asset: None,
            model_info: None,
            codename: None,
            serial_number: None,
            unit_id: [1, 2, 3, 4],
            driver_id: None,
            registry_model_id: None,
            route: None,
            capture_id: None,
            kind,
            capabilities: Some(Capabilities {
                host_switching,
                pointer: matches!(kind, DeviceKind::Mouse | DeviceKind::Trackball),
                ..Capabilities::default()
            }),
            light_capabilities: None,
            slot: 1,
            online: true,
            battery: None,
        }
    }

    #[test]
    fn only_change_host_devices_are_compatible() {
        assert!(is_compatible_target(&record(DeviceKind::Mouse, true)));
        assert!(is_compatible_target(&record(DeviceKind::Trackball, true)));
        assert!(!is_compatible_target(&record(DeviceKind::Mouse, false)));
        assert!(is_compatible_target(&record(DeviceKind::Keyboard, true)));
    }

    fn paired_device(
        slot: u8,
        name: &str,
        kind: DeviceKind,
        unit_id: [u8; 4],
        host_switching: bool,
    ) -> PairedDevice {
        PairedDevice {
            slot,
            codename: Some(name.into()),
            wpid: None,
            kind,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb000 + u16::from(slot), 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities {
                buttons: kind == DeviceKind::Keyboard,
                host_switching,
                host_switch_controls: kind == DeviceKind::Keyboard && host_switching,
                pointer: matches!(kind, DeviceKind::Mouse | DeviceKind::Trackball),
                ..Capabilities::default()
            }),
        }
    }

    fn host_switch_inventory() -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Bolt Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("test-receiver".into()),
            },
            paired: vec![
                paired_device(1, "MX Keys", DeviceKind::Keyboard, [1, 2, 3, 4], true),
                paired_device(2, "MX Master 4", DeviceKind::Mouse, [5, 6, 7, 8], true),
                paired_device(
                    3,
                    "Unsupported mouse",
                    DeviceKind::Mouse,
                    [9, 10, 11, 12],
                    false,
                ),
            ],
        }
    }

    #[test]
    fn selecting_a_compatible_follower_persists_identity_and_rejects_self_links() {
        let inventory = host_switch_inventory();
        let mut config = Config::ephemeral();
        config.set_selected_device(Some("unit:01020304".into()));
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            config,
            &[inventory],
            &[],
            &AssetResolver::new(),
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        );
        while receiver.try_recv().is_ok() {}

        let targets = state.host_switch_target_devices();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].config_key, "unit:05060708");
        assert_eq!(targets[0].display_name, "MX Master 4");

        assert!(matches!(
            state.set_host_switch_target_enabled("unit:05060708", true),
            HostSwitchTargetUpdate::Persisted(_)
        ));
        assert_eq!(
            state.config.devices["unit:01020304"].host_switch_targets,
            ["unit:05060708"]
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::services::ipc::Command::ReloadConfig)
        ));

        assert_eq!(
            state.set_host_switch_target_enabled("unit:01020304", true),
            HostSwitchTargetUpdate::Unchanged
        );
        assert_eq!(
            state.config.devices["unit:01020304"].host_switch_targets,
            ["unit:05060708"]
        );
        assert!(receiver.try_recv().is_err());

        assert_eq!(
            state.set_host_switch_target_enabled("unit:090a0b0c", true),
            HostSwitchTargetUpdate::Unchanged
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn persistence_failure_reports_rollback_and_restores_the_selection() {
        let inventory = host_switch_inventory();
        let mut config = Config::ephemeral();
        config.set_selected_device(Some("unit:01020304".into()));
        let temp = tempfile::tempdir().expect("temporary config directory");
        let path = temp.path().join("config.toml");
        let (_, file) = ConfigFile::load_from_path(&path).expect("new tracked config");
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            config,
            &[inventory],
            &[],
            &AssetResolver::new(),
            &[],
            ConfigPersistence::UserFile(file),
            commands,
        );
        while receiver.try_recv().is_ok() {}
        std::fs::write(&path, "schema_version = 5\n").expect("create a conflicting edit");

        assert!(matches!(
            state.set_host_switch_target_enabled("unit:05060708", true),
            HostSwitchTargetUpdate::RolledBack
        ));
        assert!(
            state.config.devices["unit:01020304"]
                .host_switch_targets
                .is_empty(),
            "the failed selection must be restored to the last persisted value"
        );
        assert!(
            state
                .config_issue()
                .is_some_and(|issue| issue.contains("changed on disk"))
        );
        assert!(
            receiver.try_recv().is_err(),
            "the agent must not reload a config that failed to persist"
        );
    }

    #[test]
    fn target_selection_is_idempotent() {
        let mut targets = vec!["mouse-a".to_string()];
        assert!(!set_target_enabled(&mut targets, "mouse-a", true));
        assert!(set_target_enabled(&mut targets, "mouse-b", true));
        assert!(!set_target_enabled(&mut targets, "mouse-b", true));
        assert_eq!(targets, ["mouse-a", "mouse-b"]);

        assert!(set_target_enabled(&mut targets, "mouse-a", false));
        assert!(!set_target_enabled(&mut targets, "mouse-a", false));
        assert_eq!(targets, ["mouse-b"]);
    }
}

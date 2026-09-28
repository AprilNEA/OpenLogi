//! Easy-Switch follower configuration projected into the device UI.

use openlogi_core::device::DeviceKind;

use super::{AppState, DeviceRecord, StateEvent, StateEvents};

/// One device that can follow the selected keyboard's host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostSwitchTargetDevice {
    pub(crate) config_key: String,
    pub(crate) display_name: String,
    pub(crate) kind: DeviceKind,
    pub(crate) online: bool,
    pub(crate) selected: bool,
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

        let mut targets: Vec<_> = self
            .devices()
            .iter()
            .filter_map(|record| {
                let config_key = record.persistent_config_key()?;
                let is_selected = selected.iter().any(|key| key == config_key);
                if config_key == keyboard_key || (!is_selected && !is_compatible_target(record)) {
                    return None;
                }
                Some(HostSwitchTargetDevice {
                    selected: is_selected,
                    config_key: config_key.to_string(),
                    display_name: record.display_name.clone(),
                    kind: record.kind,
                    online: record.online,
                })
            })
            .collect();
        // A saved link must remain removable even when its inventory record or
        // measured capability disappears. Prefer the saved name when available.
        for key in selected {
            if key == keyboard_key || targets.iter().any(|target| &target.config_key == key) {
                continue;
            }
            let config = self.config.devices.get(key);
            let identity = config.and_then(|config| config.identity.as_ref());
            targets.push(HostSwitchTargetDevice {
                config_key: key.clone(),
                display_name: config
                    .and_then(|config| config.custom_name.clone())
                    .or_else(|| identity.map(|identity| identity.display_name.clone()))
                    .unwrap_or_else(|| key.clone()),
                kind: identity.map_or(DeviceKind::Unknown, |identity| identity.kind),
                online: false,
                selected: true,
            });
        }
        targets
    }

    /// Add or remove one follower and reload the agent when persistence wins.
    pub(crate) fn set_host_switch_target_enabled(
        &mut self,
        target_key: &str,
        enabled: bool,
    ) -> StateEvents {
        let Some(keyboard) = self.current_record() else {
            return StateEvents::none();
        };
        let Some(keyboard_key) = keyboard.persistent_config_key().map(str::to_string) else {
            return StateEvents::none();
        };
        let event_key = keyboard.device_key();
        if !keyboard
            .capabilities
            .is_some_and(|caps| caps.host_switch_controls)
            || (enabled
                && (target_key == keyboard_key.as_str()
                    || !self.devices().iter().any(|record| {
                        record.persistent_config_key() == Some(target_key)
                            && is_compatible_target(record)
                    })))
        {
            return StateEvents::none();
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
            StateEvents::none()
        } else if self.persist_and_reload("Easy-Switch follower") {
            StateEvent::DeviceConfigChanged(event_key).into()
        } else {
            StateEvent::SettingsChanged.into()
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
        super::ConfigPersistence, AppState, DeviceRecord, StateEvent, StateEvents,
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
        let mut state = AppState::new(super::super::Sources {
            config,
            inventories: &[inventory],
            standalone: &[],
            resolver: &AssetResolver::new(),
            cameras: &[],
            persistence: ConfigPersistence::MemoryOnly,
            ipc_commands: commands,
        });
        while receiver.try_recv().is_ok() {}

        let targets = state.host_switch_target_devices();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].config_key, "unit:05060708");
        assert_eq!(targets[0].display_name, "MX Master 4");

        let keyboard = state.current_record().unwrap().device_key();
        assert_eq!(
            state.set_host_switch_target_enabled("unit:05060708", true),
            [StateEvent::DeviceConfigChanged(keyboard)]
        );
        assert_eq!(
            state.config.devices["unit:01020304"].host_switch_targets,
            ["unit:05060708"]
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::services::ipc::Command::ReloadConfig(_))
        ));

        assert_eq!(
            state.set_host_switch_target_enabled("unit:01020304", true),
            StateEvents::none()
        );
        assert_eq!(
            state.config.devices["unit:01020304"].host_switch_targets,
            ["unit:05060708"]
        );
        assert!(receiver.try_recv().is_err());

        assert_eq!(
            state.set_host_switch_target_enabled("unit:090a0b0c", true),
            StateEvents::none()
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn saved_missing_or_unsupported_followers_remain_removable() {
        for target_key in ["unit:11121314", "unit:090a0b0c"] {
            let inventory = host_switch_inventory();
            let mut config = Config::ephemeral();
            config.set_selected_device(Some("unit:01020304".into()));
            config
                .devices
                .entry("unit:01020304".into())
                .or_default()
                .host_switch_targets = vec![target_key.into()];
            config
                .devices
                .entry(target_key.into())
                .or_default()
                .custom_name = Some("Saved follower".into());
            let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut state = AppState::new(super::super::Sources {
                config,
                inventories: &[inventory],
                standalone: &[],
                resolver: &AssetResolver::new(),
                cameras: &[],
                persistence: ConfigPersistence::MemoryOnly,
                ipc_commands: commands,
            });
            while receiver.try_recv().is_ok() {}
            let targets = state.host_switch_target_devices();
            let saved = targets
                .iter()
                .find(|target| target.config_key == target_key)
                .expect("a saved follower must have a removable row");
            assert!(saved.selected);
            assert_eq!(saved.display_name, "Saved follower");
            if target_key == "unit:11121314" {
                assert!(!saved.online);
            }
            let keyboard = state.current_record().unwrap().device_key();
            assert_eq!(
                state.set_host_switch_target_enabled(target_key, false),
                [StateEvent::DeviceConfigChanged(keyboard)]
            );
            assert!(
                state.config.devices["unit:01020304"]
                    .host_switch_targets
                    .is_empty()
            );
            assert!(matches!(
                receiver.try_recv(),
                Ok(crate::services::ipc::Command::ReloadConfig(_))
            ));
            assert!(
                !state
                    .host_switch_target_devices()
                    .iter()
                    .any(|target| target.config_key == target_key)
            );
            assert_eq!(
                state.set_host_switch_target_enabled(target_key, true),
                StateEvents::none()
            );
            assert!(
                receiver.try_recv().is_err(),
                "unavailable targets cannot be re-enabled"
            );
        }
    }

    #[test]
    fn persistence_failure_reports_rollback_and_restores_the_selection() {
        let inventory = host_switch_inventory();
        let mut config = Config::ephemeral();
        config.set_selected_device(Some("unit:01020304".into()));
        let temp = tempfile::tempdir().expect("temporary config directory");
        let path = temp.path().join(openlogi_core::paths::CONFIG_FILE);
        let (_, file) = ConfigFile::load_from_path(&path).expect("new tracked config");
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(super::super::Sources {
            config,
            inventories: &[inventory],
            standalone: &[],
            resolver: &AssetResolver::new(),
            cameras: &[],
            persistence: ConfigPersistence::UserFile(file),
            ipc_commands: commands,
        });
        while receiver.try_recv().is_ok() {}
        std::fs::write(&path, "schema_version = 5\n").expect("create a conflicting edit");

        assert_eq!(
            state.set_host_switch_target_enabled("unit:05060708", true),
            [StateEvent::SettingsChanged]
        );
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

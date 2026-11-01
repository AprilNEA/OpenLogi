//! Linked Easy-Switch configuration for the active keyboard.

use openlogi_core::device::DeviceKind;

use super::events::StateEvents;
use super::{AppState, DeviceRecord, StateEvent};

/// One pointing device that can follow the active keyboard's Easy-Switch
/// channel. The configuration key is the stable identity used by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostSwitchTarget {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) online: bool,
    pub(crate) selected: bool,
}

/// Switching needs only `0x1814 ChangeHost`: the engine reads `0x1815
/// HostsInfo` slot names when a device has them and otherwise moves it to the
/// keyboard's slot number.
fn supports_host_switch(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.change_host)
}

impl DeviceRecord {
    /// Whether this record is a keyboard whose Easy-Switch channel other
    /// devices can follow. A measured pointer makes it a target instead.
    pub(crate) fn is_host_switch_keyboard(&self) -> bool {
        self.kind == DeviceKind::Keyboard
            && !self
                .capabilities
                .is_some_and(|capabilities| capabilities.pointer)
            && supports_host_switch(self)
    }
}

fn is_pointing_device(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.pointer)
        || matches!(
            record.kind,
            DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
        )
}

impl AppState {
    /// Easy-Switch-capable pointing devices available to follow the active
    /// keyboard. Offline known devices remain configurable and apply later.
    #[must_use]
    pub(crate) fn current_host_switch_targets(&self) -> Vec<HostSwitchTarget> {
        let Some(keyboard_key) = self
            .current_record()
            .filter(|record| record.is_host_switch_keyboard())
            .and_then(DeviceRecord::persistent_config_key)
        else {
            return Vec::new();
        };
        let selected = self.config.host_switch_targets(keyboard_key);
        self.devices
            .records
            .iter()
            .filter_map(|record| {
                let key = record.persistent_config_key()?;
                let selected = selected.iter().any(|selected| selected == key);
                // A target that lost ChangeHost stays listed while selected,
                // so it can still be removed.
                if !is_pointing_device(record) || !(selected || supports_host_switch(record)) {
                    return None;
                }
                Some(HostSwitchTarget {
                    key: key.to_string(),
                    name: record.display_name.clone(),
                    online: record.online,
                    selected,
                })
            })
            .collect()
    }

    /// Add or remove one target from the active keyboard's linked switch
    /// group, then reload the agent so its existing host-switch watcher uses
    /// the new relationship.
    pub(crate) fn commit_host_switch_target(
        &mut self,
        target_key: &str,
        selected: bool,
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        // Only a listed row whose state actually changes is written.
        let changes = self
            .current_host_switch_targets()
            .iter()
            .any(|target| target.key == target_key && target.selected != selected);
        let Some(keyboard_key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .filter(|_| changes)
            .map(str::to_string)
        else {
            return events;
        };
        self.config.edit(|config| {
            config.set_host_switch_target(&keyboard_key, target_key, selected);
        });
        self.persist_and_reload("Easy-Switch target");
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
    use crate::state::DeviceKey;
    use crate::state::{ConfigPersistence, Sources};
    use openlogi_core::config::Config;
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };

    const CHANGE_HOST: u16 = 0x1814;
    const HOSTS_INFO: u16 = 0x1815;

    fn inventory(
        name: &str,
        kind: DeviceKind,
        unit_id: [u8; 4],
        host_features: &[u16],
    ) -> DeviceInventory {
        let measured = Capabilities::from_feature_ids(host_features);
        let mut capabilities = Capabilities::presumed_from_kind(kind);
        capabilities.easy_switch = measured.easy_switch;
        capabilities.change_host = measured.change_host;
        DeviceInventory {
            receiver: ReceiverInfo {
                name: format!("{name} receiver"),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some(format!("receiver-{name}")),
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some(name.to_string()),
                wpid: None,
                kind,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id,
                    transports: DeviceTransports::default(),
                    model_ids: [0, 0, 0],
                    extended_model_id: 0,
                }),
                capabilities: Some(capabilities),
                settings_revision: 0,
            }],
        }
    }

    fn state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        state_with_keyboard(&[CHANGE_HOST, HOSTS_INFO])
    }

    fn state_with_keyboard(
        keyboard_features: &[u16],
    ) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let inventories = [
            inventory(
                "MX Keys",
                DeviceKind::Keyboard,
                [1, 2, 3, 4],
                keyboard_features,
            ),
            inventory(
                "MX Master",
                DeviceKind::Mouse,
                [5, 6, 7, 8],
                &[CHANGE_HOST, HOSTS_INFO],
            ),
            inventory("Basic Mouse", DeviceKind::Mouse, [9, 10, 11, 12], &[]),
            // The MX Master 3 has ChangeHost but no HostsInfo.
            inventory(
                "MX Master 3",
                DeviceKind::Mouse,
                [13, 14, 15, 16],
                &[CHANGE_HOST],
            ),
            inventory(
                "Names Only",
                DeviceKind::Mouse,
                [17, 18, 19, 20],
                &[HOSTS_INFO],
            ),
        ];
        let mut config = Config::ephemeral();
        config.set_selected_device(Some("unit:01020304".to_string()));
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::new(Sources {
            config,
            inventories: &inventories,
            standalone: &[],
            resolver: &AssetResolver::new(),
            cameras: &[],
            persistence: ConfigPersistence::MemoryOnly,
            ipc_commands: commands,
        });
        (state, receiver)
    }

    #[test]
    fn only_change_host_pointing_devices_are_offered() {
        let (state, _receiver) = state();

        assert_eq!(
            state.current_host_switch_targets(),
            [
                HostSwitchTarget {
                    key: "unit:05060708".to_string(),
                    name: "MX Master".to_string(),
                    online: true,
                    selected: false,
                },
                HostSwitchTarget {
                    key: "unit:0d0e0f10".to_string(),
                    name: "MX Master 3".to_string(),
                    online: true,
                    selected: false,
                },
            ]
        );
    }

    #[test]
    fn a_keyboard_needs_change_host_but_not_hosts_info() {
        let (state, _receiver) = state_with_keyboard(&[CHANGE_HOST]);
        assert!(!state.current_host_switch_targets().is_empty());

        let (state, _receiver) = state_with_keyboard(&[HOSTS_INFO]);
        assert!(state.current_host_switch_targets().is_empty());
    }

    #[test]
    fn target_toggle_persists_and_reloads_the_agent() {
        let (mut state, mut receiver) = state();
        while receiver.try_recv().is_ok() {}

        let events = state.commit_host_switch_target("unit:05060708", true);

        assert_eq!(
            events,
            [StateEvent::DeviceConfigChanged(DeviceKey::from(
                "unit:01020304"
            ))]
        );
        assert_eq!(
            state.config.host_switch_targets("unit:01020304"),
            ["unit:05060708"]
        );
        assert!(matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn an_old_unsupported_target_stays_visible_and_can_be_removed() {
        let (mut state, mut receiver) = state();
        while receiver.try_recv().is_ok() {}
        state.config.edit(|config| {
            config.set_host_switch_target("unit:01020304", "unit:090a0b0c", true);
        });

        let targets = state.current_host_switch_targets();
        assert!(targets.iter().any(|target| {
            target.key == "unit:090a0b0c" && target.name == "Basic Mouse" && target.selected
        }));

        let _ = state.commit_host_switch_target("unit:090a0b0c", false);
        assert!(
            !state
                .config
                .host_switch_targets("unit:01020304")
                .iter()
                .any(|target| target == "unit:090a0b0c")
        );
        assert!(matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))));
    }
}

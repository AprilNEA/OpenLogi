use openlogi_core::config::MonitorInputAssignment;
use openlogi_core::device::DeviceKind;

use super::{
    AppState, DeviceRecord, HostSwitchKeyboardDevice, HostSwitchTargetDevice, MonitorDiscovery,
};

impl AppState {
    #[must_use]
    pub fn monitor_discovery(&self) -> &MonitorDiscovery {
        &self.monitor_discovery
    }

    pub fn set_monitor_loading(&mut self) {
        self.monitor_discovery = MonitorDiscovery::Loading;
    }

    pub fn store_monitors(&mut self, result: Result<Vec<openlogi_monitor::MonitorInfo>, String>) {
        self.monitor_discovery = match result {
            Ok(monitors) => MonitorDiscovery::Ready(monitors),
            Err(error) => MonitorDiscovery::Failed(error),
        };
    }

    #[must_use]
    pub fn host_monitor_input(&self, host: u8, monitor_id: &str) -> Option<u32> {
        let key = self.selected_host_switch_keyboard_key()?;
        self.config
            .devices
            .get(key)
            .and_then(|device| device.host_switch_monitor_inputs.get(&host.to_string()))
            .and_then(|assignments| {
                assignments
                    .iter()
                    .find(|assignment| assignment.monitor_id == monitor_id)
                    .map(|assignment| assignment.input)
            })
    }

    #[must_use]
    pub fn host_monitor_enabled(&self) -> bool {
        let Some(key) = self.selected_host_switch_keyboard_key() else {
            return true;
        };
        self.config
            .devices
            .get(key)
            .is_none_or(|device| device.host_switch_monitor_enabled)
    }

    pub fn set_host_monitor_enabled(&mut self, enabled: bool) {
        let Some(key) = self.writable_host_switch_keyboard_key().map(str::to_string) else {
            return;
        };
        let changed = self.config.edit(|config| {
            let device = config.devices.entry(key).or_default();
            if device.host_switch_monitor_enabled == enabled {
                return false;
            }
            device.host_switch_monitor_enabled = enabled;
            true
        });
        if !changed {
            return;
        }
        self.persist_and_reload("host-switch monitor enabled");
    }

    #[must_use]
    pub fn host_switch_keyboard_devices(&self) -> Vec<HostSwitchKeyboardDevice> {
        let selected = self.selected_host_switch_keyboard_key();
        self.devices()
            .iter()
            .filter(|record| record.kind == DeviceKind::Keyboard)
            .filter_map(|record| {
                let config_key = record.persistent_config_key()?.to_string();
                Some(HostSwitchKeyboardDevice {
                    selected: selected == Some(config_key.as_str()),
                    config_key,
                    display_name: record.display_name.clone(),
                    online: record.online,
                })
            })
            .collect()
    }

    pub fn set_host_switch_keyboard_key(&mut self, config_key: String) {
        let exists = self.devices().iter().any(|record| {
            record.kind == DeviceKind::Keyboard
                && record.online
                && record.persistent_config_key() == Some(config_key.as_str())
        });
        if !exists || self.host_switch_keyboard_key.as_deref() == Some(config_key.as_str()) {
            return;
        }
        self.host_switch_keyboard_key = Some(config_key);
    }

    #[must_use]
    pub fn host_switch_keyboard_name(&self) -> Option<String> {
        let key = self.selected_host_switch_keyboard_key()?;
        self.devices()
            .iter()
            .find(|record| record.persistent_config_key() == Some(key))
            .map(|record| record.display_name.clone())
            .or_else(|| {
                self.config
                    .devices
                    .get(key)
                    .and_then(|device| device.identity.as_ref())
                    .map(|identity| identity.display_name.clone())
            })
    }

    #[must_use]
    pub fn host_switch_target_devices(&self) -> Vec<HostSwitchTargetDevice> {
        let Some(keyboard_key) = self.writable_host_switch_keyboard_key() else {
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
            .filter(|record| is_follow_target_kind(record.kind))
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

    pub fn set_host_switch_target_enabled(&mut self, target_key: &str, enabled: bool) {
        let Some(keyboard_key) = self.writable_host_switch_keyboard_key().map(str::to_string)
        else {
            return;
        };
        let changed = self.config.edit(|config| {
            let targets = &mut config
                .devices
                .entry(keyboard_key)
                .or_default()
                .host_switch_targets;
            let contains = targets.iter().any(|key| key == target_key);
            match (enabled, contains) {
                (true, false) => targets.push(target_key.to_string()),
                (false, true) => targets.retain(|key| key != target_key),
                _ => return false,
            }
            true
        });
        if !changed {
            return;
        }
        self.persist_and_reload("host-switch target device");
    }

    pub fn commit_host_monitor_input(&mut self, host: u8, monitor_id: String, input: u32) {
        let Some(key) = self.writable_host_switch_keyboard_key().map(str::to_string) else {
            return;
        };
        let changed = self.config.edit(|config| {
            let host_key = host.to_string();
            let entry = config
                .devices
                .entry(key)
                .or_default()
                .host_switch_monitor_inputs
                .entry(host_key)
                .or_default();
            if let Some(existing) = entry
                .iter_mut()
                .find(|assignment| assignment.monitor_id == monitor_id)
            {
                if existing.input == input {
                    return false;
                }
                existing.input = input;
            } else {
                entry.push(MonitorInputAssignment { monitor_id, input });
            }
            true
        });
        if !changed {
            return;
        }
        self.persist_and_reload("host-switch monitor input");
    }

    #[must_use]
    pub fn host_switch_warning(&self) -> Option<&str> {
        self.host_switch_warning.as_deref()
    }

    pub fn set_host_switch_warning(&mut self, warning: Option<String>) -> bool {
        if self.host_switch_warning == warning {
            return false;
        }
        self.host_switch_warning = warning;
        true
    }

    pub(super) fn reconcile_host_switch_keyboard_key_for(&mut self, list: &[DeviceRecord]) -> bool {
        let selected_exists = self
            .host_switch_keyboard_key
            .as_deref()
            .is_some_and(|key| list.iter().any(|record| is_keyboard_with_key(record, key)));
        let next = if selected_exists {
            self.host_switch_keyboard_key.clone()
        } else {
            only_online_keyboard_key(list)
        };
        if self.host_switch_keyboard_key == next {
            return false;
        }
        self.host_switch_keyboard_key = next;
        true
    }

    fn selected_host_switch_keyboard_key(&self) -> Option<&str> {
        self.host_switch_keyboard_key.as_deref()
    }

    fn writable_host_switch_keyboard_key(&self) -> Option<&str> {
        let key = self.selected_host_switch_keyboard_key()?;
        self.devices()
            .iter()
            .any(|record| is_keyboard_with_key(record, key) && record.online)
            .then_some(key)
    }
}

fn is_follow_target_kind(kind: DeviceKind) -> bool {
    matches!(
        kind,
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
    )
}

fn is_keyboard_with_key(record: &DeviceRecord, key: &str) -> bool {
    record.kind == DeviceKind::Keyboard && record.persistent_config_key() == Some(key)
}

fn only_online_keyboard_key(list: &[DeviceRecord]) -> Option<String> {
    let mut keys = list
        .iter()
        .filter(|record| record.kind == DeviceKind::Keyboard && record.online)
        .filter_map(DeviceRecord::persistent_config_key);
    let only = keys.next()?;
    keys.next().is_none().then(|| only.to_string())
}

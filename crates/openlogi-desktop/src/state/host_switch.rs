//! Easy-Switch host-switch links: which keyboards a pointing device follows.
//!
//! `host_switch_targets` is stored on the *keyboard*'s config entry (a list of
//! the pointing devices that follow it — see
//! [`openlogi_core::config::device::DeviceConfig::host_switch_targets`]), but
//! it reads more naturally from the mouse's side ("which keyboard do I follow
//! when it switches hosts?"), so the panel is built here and the write is
//! aimed at the other device's config entry instead of the current one.

use openlogi_core::device::DeviceKind;

use super::AppState;

/// One keyboard the active pointing device could follow, and whether it
/// already does.
pub struct HostSwitchCandidate {
    pub config_key: String,
    pub display_name: String,
    pub following: bool,
}

impl AppState {
    /// Keyboards the active device could follow on host switch, in device
    /// gallery order. Empty when the active device isn't a persistent
    /// pointing device that measured `ChangeHost` support (HID++
    /// `0x1814`/`0x1815`), and each candidate keyboard is further filtered to
    /// ones that measured an armable host-switch control in their `0x1b04`
    /// control table — gating on kind alone would let an unsupported
    /// pairing be toggled on with no way for the agent to ever act on it.
    #[must_use]
    pub fn host_switch_candidates(&self) -> Vec<HostSwitchCandidate> {
        let Some(target_key) = self
            .current_record()
            .filter(|record| matches!(record.kind, DeviceKind::Mouse | DeviceKind::Trackball))
            .filter(|record| record.is_persistent())
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|caps| caps.host_switch_target)
            })
            .map(|record| record.config_key.clone())
        else {
            return Vec::new();
        };
        self.devices()
            .iter()
            .filter(|record| record.kind == DeviceKind::Keyboard && record.is_persistent())
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|caps| caps.host_switch_source)
            })
            .map(|record| HostSwitchCandidate {
                config_key: record.config_key.clone(),
                display_name: record.display_name.clone(),
                following: self
                    .config
                    .host_switch_targets(&record.config_key)
                    .iter()
                    .any(|key| key == &target_key),
            })
            .collect()
    }

    /// Add or remove the active pointing device from `keyboard_key`'s
    /// host-switch targets. No-op when the active device isn't persistent.
    pub fn set_host_switch_follow(&mut self, keyboard_key: &str, follow: bool) {
        let Some(target_key) = self
            .current_record()
            .filter(|record| record.is_persistent())
            .map(|record| record.config_key.clone())
        else {
            return;
        };
        self.config.edit(|config| {
            let mut targets = config.host_switch_targets(keyboard_key);
            let already_present = targets.iter().any(|key| key == &target_key);
            if follow && !already_present {
                targets.push(target_key.clone());
            } else if !follow {
                targets.retain(|key| key != &target_key);
            }
            config.set_host_switch_targets(keyboard_key, targets);
        });
        self.persist_and_reload("host-switch target");
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::config::Config;
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };

    use super::super::ConfigPersistence;
    use super::AppState;
    use crate::services::assets::AssetResolver;

    fn direct_device(
        unit_id: [u8; 4],
        kind: DeviceKind,
        name: &str,
        supports_host_switch: bool,
    ) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: name.to_string(),
                vendor_id: 0x046d,
                product_id: 0xb023,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
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
                    model_ids: [0xb034, 0, 0],
                    extended_model_id: 2,
                }),
                capabilities: Some(Capabilities {
                    host_switch_target: supports_host_switch,
                    host_switch_source: supports_host_switch,
                    ..Capabilities::presumed_from_kind(kind)
                }),
            }],
        }
    }

    /// A state with one persistent mouse (the active device) and one
    /// persistent keyboard, both measuring host-switch support, so a
    /// host-switch link can be formed between them.
    fn state_with_a_mouse_and_a_keyboard() -> AppState {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mouse = direct_device(
            [0x01, 0x02, 0x03, 0x04],
            DeviceKind::Mouse,
            "MX Master 3S",
            true,
        );
        let keyboard = direct_device(
            [0x05, 0x06, 0x07, 0x08],
            DeviceKind::Keyboard,
            "MX Keys S",
            true,
        );
        AppState::with_runtime(
            Config::ephemeral(),
            &[mouse, keyboard],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        )
    }

    #[test]
    fn candidates_list_persistent_keyboards_only_for_the_active_mouse() {
        let state = state_with_a_mouse_and_a_keyboard();
        assert!(
            state
                .devices()
                .iter()
                .any(|record| record.display_name == "MX Master 3S"),
            "the mouse should be the active device by default"
        );

        let candidates = state.host_switch_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display_name, "MX Keys S");
        assert!(!candidates[0].following);
    }

    #[test]
    fn following_toggles_the_keyboards_host_switch_targets() {
        let mut state = state_with_a_mouse_and_a_keyboard();
        let keyboard_key = state.host_switch_candidates()[0].config_key.clone();

        state.set_host_switch_follow(&keyboard_key, true);
        assert!(state.host_switch_candidates()[0].following);

        state.set_host_switch_follow(&keyboard_key, false);
        assert!(!state.host_switch_candidates()[0].following);
    }

    #[test]
    fn a_mouse_without_change_host_support_lists_no_candidates() {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mouse = direct_device([0x01, 0x02, 0x03, 0x04], DeviceKind::Mouse, "M185", false);
        let keyboard = direct_device(
            [0x05, 0x06, 0x07, 0x08],
            DeviceKind::Keyboard,
            "MX Keys S",
            true,
        );
        let state = AppState::with_runtime(
            Config::ephemeral(),
            &[mouse, keyboard],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        );

        assert!(state.host_switch_candidates().is_empty());
    }

    #[test]
    fn a_keyboard_without_an_armable_host_switch_control_is_not_a_candidate() {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mouse = direct_device(
            [0x01, 0x02, 0x03, 0x04],
            DeviceKind::Mouse,
            "MX Master 3S",
            true,
        );
        let keyboard = direct_device(
            [0x05, 0x06, 0x07, 0x08],
            DeviceKind::Keyboard,
            "K120",
            false,
        );
        let state = AppState::with_runtime(
            Config::ephemeral(),
            &[mouse, keyboard],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        );

        assert!(state.host_switch_candidates().is_empty());
    }
}

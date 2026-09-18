//! Per-keyboard Fn-lock (HID++ `0x40a2`/`0x40a3`) toggle.

use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::AppState;

impl AppState {
    /// Whether the active device reports Fn-lock support (HID++ `0x40a2` or
    /// `0x40a3` in its feature table). Gating on the measured/last-known
    /// capability rather than [`openlogi_core::device::DeviceKind`] keeps the
    /// toggle from showing — and persisting a state the hardware never
    /// applies — on a keyboard that doesn't expose either feature.
    #[must_use]
    pub fn current_device_supports_fn_lock(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.fn_lock)
    }

    /// The active keyboard's persisted Fn-lock state. `false` when unset —
    /// the keyboard's own onboard state may differ until the agent first
    /// applies this, matching every other "unset" default in this panel.
    #[must_use]
    pub fn current_fn_lock(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.fn_lock(key).unwrap_or(false))
    }

    /// Set the active keyboard's Fn-lock state, persist it, and reload the
    /// agent so it writes HID++ `0x40a3`. No-op when no device is selected,
    /// the active device doesn't support Fn-lock, or it has no persistent
    /// config key.
    pub fn commit_fn_lock(&mut self, fn_lock: bool) {
        if !self.current_device_supports_fn_lock() {
            debug!("active device does not support Fn-lock — change ignored");
            return;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — Fn-lock change ignored");
            return;
        };
        self.config.edit(|config| config.set_fn_lock(&key, fn_lock));
        self.persist_and_reload("Fn-lock");
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

    fn direct_keyboard() -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Keys S".to_string(),
                vendor_id: 0x046d,
                product_id: 0xb378,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
                codename: Some("MX Keys S".to_string()),
                wpid: None,
                kind: DeviceKind::Keyboard,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id: [0x0a, 0x0b, 0x0c, 0x0d],
                    transports: DeviceTransports::default(),
                    model_ids: [0xb378, 0, 0],
                    extended_model_id: 1,
                }),
                capabilities: Some(Capabilities {
                    fn_lock: true,
                    ..Capabilities::presumed_from_kind(DeviceKind::Keyboard)
                }),
            }],
        }
    }

    fn state_with_a_keyboard() -> AppState {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        AppState::with_runtime(
            Config::ephemeral(),
            &[direct_keyboard()],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        )
    }

    #[test]
    fn fn_lock_defaults_to_false_and_toggles() {
        let mut state = state_with_a_keyboard();
        assert!(state.current_device_supports_fn_lock());
        assert!(!state.current_fn_lock());

        state.commit_fn_lock(true);
        assert!(state.current_fn_lock());

        state.commit_fn_lock(false);
        assert!(!state.current_fn_lock());
    }

    #[test]
    fn a_keyboard_without_fn_lock_capability_does_not_support_the_toggle() {
        let mut inventory = direct_keyboard();
        inventory.paired[0].capabilities =
            Some(Capabilities::presumed_from_kind(DeviceKind::Keyboard));
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            Config::ephemeral(),
            &[inventory],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        );

        assert!(!state.current_device_supports_fn_lock());

        // A write attempt on an unsupported keyboard must not persist —
        // otherwise the UI would report a state the hardware never applies.
        state.commit_fn_lock(true);
        assert!(!state.current_fn_lock());
    }
}

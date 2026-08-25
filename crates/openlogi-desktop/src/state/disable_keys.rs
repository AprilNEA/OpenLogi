//! Guarded Disable Keys reads, writes, persistence, and recovery.

use std::collections::BTreeSet;

use gpui::{App, Context};
use openlogi_core::config::DisableKey;
use openlogi_core::hid::{DisableKeysMask, DisableKeysState, WriteError};

use crate::services::ipc::{Command, ConfigReloadContext, DisableKeysRequestContext};

use super::{AppState, DeviceKey, DisableKeysLoad, Load, StateEvent};

/// Route-independent identity retained across disconnects during recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisableKeysRecoveryToken {
    pub(crate) key: DeviceKey,
    pub(crate) transaction_id: u64,
}

/// Persistence phase for one device's confirmed Disable Keys transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum DisableKeysPersistenceStatus {
    #[default]
    Idle,
    Applying(DisableKeysRequestContext),
    AppliedNotSaved {
        recovery: DisableKeysRecoveryToken,
        confirmed: DisableKeysState,
    },
    AwaitingReload(DisableKeysRequestContext),
    SavedNotReloaded(DisableKeysRequestContext),
    SavedNotReloadedDetached(DisableKeysRecoveryToken),
}

/// Per-device Disable Keys transaction state.
#[derive(Debug, Default)]
pub(super) struct DisableKeysDeviceState {
    pub(super) persistence: DisableKeysPersistenceStatus,
    pub(super) error: Option<String>,
}

impl AppState {
    pub(super) fn load_current_disable_keys(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self.current_record().and_then(|record| {
            let supported = record.capabilities.unwrap_or_default().disable_keys;
            (record.online && supported)
                .then(|| {
                    record
                        .route
                        .clone()
                        .map(|route| (record.device_key(), route))
                })
                .flatten()
        }) else {
            return;
        };
        self.disable_keys_reads
            .ensure(key, route, self.ipc_sender(), cx);
    }

    pub(crate) fn retry_disable_keys_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.disable_keys_reads.retry(&key);
            cx.emit(StateEvent::DisableKeysChanged(key));
        });
    }

    pub(crate) fn update_disable_keys(cx: &mut App, desired: DisableKeysMask) {
        Self::update(cx, |state, cx| {
            let Some(key) = state.begin_disable_keys_write(desired) else {
                return;
            };
            cx.emit(StateEvent::DisableKeysChanged(key));
        });
    }

    pub(crate) fn retry_disable_keys_save(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            if state.retry_disable_keys_save_inner(&key) {
                cx.emit(StateEvent::DisableKeysChanged(key));
            }
        });
    }

    pub(crate) fn retry_disable_keys_reload(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            if state.retry_disable_keys_reload_inner(&key) {
                cx.emit(StateEvent::DisableKeysChanged(key));
            }
        });
    }

    fn begin_disable_keys_write(&mut self, desired: DisableKeysMask) -> Option<DeviceKey> {
        if !self.config.is_writable() {
            return None;
        }
        let record = self.current_record()?;
        if !record.online || !record.is_persistent() {
            return None;
        }
        let key = record.device_key();
        if !matches!(self.disable_keys_reads.load(&key), Load::Ready(_)) {
            return None;
        }
        let context = self.allocate_disable_keys_context(&key)?;
        let state = self.devices.runtime.entry(key.clone()).or_default();
        if !matches!(
            state.disable_keys.persistence,
            DisableKeysPersistenceStatus::Idle
        ) {
            return None;
        }
        state.disable_keys.persistence = DisableKeysPersistenceStatus::Applying(context.clone());
        state.disable_keys.error = None;
        if !self.send_ipc(Command::SetDisableKeys(context, desired)) {
            let state = self.devices.runtime.entry(key.clone()).or_default();
            state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
            state.disable_keys.error = Some("agent is unavailable".into());
        }
        Some(key)
    }

    pub(crate) fn apply_disable_keys_write_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<DisableKeysState, WriteError>,
    ) -> bool {
        let current = match self.disable_keys_status(&context.key) {
            Some(DisableKeysPersistenceStatus::Applying(current)) => Some(current),
            _ => None,
        };
        if !transaction_fences_match(
            self.record_route(&context.key),
            self.disable_keys_reads.generation(&context.key),
            current,
            &context,
        ) {
            return false;
        }
        let key = context.key.clone();
        let result = match result {
            Ok(confirmed) => confirmed,
            Err(error) => {
                let state = self.devices.runtime.entry(key).or_default();
                state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
                state.disable_keys.error = Some(error.to_string());
                return true;
            }
        };

        self.disable_keys_reads.set_confirmed(&key, result);
        let known = confirmed_known_set(result);
        self.config
            .edit(|config| config.set_disabled_keys(key.as_str(), known));
        if let Err(error) = self.config.persist_feature("disabled keys") {
            let state = self.devices.runtime.entry(key.clone()).or_default();
            state.disable_keys.persistence = DisableKeysPersistenceStatus::AppliedNotSaved {
                recovery: DisableKeysRecoveryToken {
                    key,
                    transaction_id: context.request_id,
                },
                confirmed: result,
            };
            state.disable_keys.error = Some(error);
            return true;
        }

        let state = self.devices.runtime.entry(key.clone()).or_default();
        state.disable_keys.persistence =
            DisableKeysPersistenceStatus::AwaitingReload(context.clone());
        state.disable_keys.error = None;
        if !self.send_ipc(Command::ReloadConfig(ConfigReloadContext::DisableKeys(
            context.clone(),
        ))) {
            let state = self.devices.runtime.entry(key).or_default();
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::SavedNotReloaded(context);
            state.disable_keys.error = Some("agent is unavailable".into());
        }
        true
    }

    pub(crate) fn apply_disable_keys_reload_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<(), openlogi_ipc::ConfigReloadError>,
    ) -> bool {
        let current = match self.disable_keys_status(&context.key) {
            Some(DisableKeysPersistenceStatus::AwaitingReload(current)) => Some(current),
            _ => None,
        };
        if !transaction_fences_match(
            self.record_route(&context.key),
            self.disable_keys_reads.generation(&context.key),
            current,
            &context,
        ) {
            return false;
        }
        let state = self.devices.runtime.entry(context.key.clone()).or_default();
        match result {
            Ok(()) => {
                state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
                state.disable_keys.error = None;
            }
            Err(error) => {
                state.disable_keys.persistence =
                    DisableKeysPersistenceStatus::SavedNotReloaded(context);
                state.disable_keys.error = Some(error.message);
            }
        }
        true
    }

    fn retry_disable_keys_save_inner(&mut self, key: &DeviceKey) -> bool {
        let Some(DisableKeysPersistenceStatus::AppliedNotSaved {
            recovery,
            confirmed,
        }) = self.disable_keys_status(key).cloned()
        else {
            return false;
        };
        if let Err(error) = self.config.refresh_feature() {
            self.devices
                .runtime
                .entry(key.clone())
                .or_default()
                .disable_keys
                .error = Some(error);
            return true;
        }
        let known = confirmed_known_set(confirmed);
        self.config
            .edit(|config| config.set_disabled_keys(recovery.key.as_str(), known));
        if let Err(error) = self.config.persist_feature("disabled keys recovery") {
            self.devices
                .runtime
                .entry(key.clone())
                .or_default()
                .disable_keys
                .error = Some(error);
            return true;
        }

        let next = self.allocate_disable_keys_context(key);
        let state = self.devices.runtime.entry(key.clone()).or_default();
        state.disable_keys.error = None;
        if let Some(context) = next {
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::AwaitingReload(context.clone());
            self.send_ipc(Command::ReloadConfig(ConfigReloadContext::DisableKeys(
                context,
            )));
        } else {
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::SavedNotReloadedDetached(recovery);
        }
        true
    }

    fn retry_disable_keys_reload_inner(&mut self, key: &DeviceKey) -> bool {
        let recovery = match self.disable_keys_status(key) {
            Some(DisableKeysPersistenceStatus::SavedNotReloaded(context)) => {
                DisableKeysRecoveryToken {
                    key: context.key.clone(),
                    transaction_id: context.request_id,
                }
            }
            Some(DisableKeysPersistenceStatus::SavedNotReloadedDetached(recovery)) => {
                recovery.clone()
            }
            _ => return false,
        };
        let Some(context) = self.allocate_disable_keys_context(&recovery.key) else {
            let state = self.devices.runtime.entry(key.clone()).or_default();
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::SavedNotReloadedDetached(recovery);
            return true;
        };
        let state = self.devices.runtime.entry(key.clone()).or_default();
        state.disable_keys.persistence =
            DisableKeysPersistenceStatus::AwaitingReload(context.clone());
        state.disable_keys.error = None;
        self.send_ipc(Command::ReloadConfig(ConfigReloadContext::DisableKeys(
            context,
        )));
        true
    }

    fn allocate_disable_keys_context(
        &mut self,
        key: &DeviceKey,
    ) -> Option<DisableKeysRequestContext> {
        let record = self
            .devices
            .records
            .iter()
            .find(|record| record.device_key() == *key && record.online)?;
        let route = record.route.clone()?;
        let route_generation = self.disable_keys_reads.generation(key)?;
        let request_id = self.next_disable_keys_request_id;
        self.next_disable_keys_request_id = self.next_disable_keys_request_id.saturating_add(1);
        Some(DisableKeysRequestContext {
            key: key.clone(),
            route,
            route_generation,
            request_id,
        })
    }

    fn record_route(&self, key: &DeviceKey) -> Option<&openlogi_core::hid::DeviceRoute> {
        self.devices
            .records
            .iter()
            .find(|record| record.device_key() == *key)
            .and_then(|record| record.route.as_ref())
    }

    pub(crate) fn invalidate_disable_keys(&mut self, key: &DeviceKey) {
        self.disable_keys_reads.remove(key);
        let Some(state) = self.devices.runtime.get_mut(key) else {
            return;
        };
        match state.disable_keys.persistence.clone() {
            DisableKeysPersistenceStatus::Applying(_) => {
                state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
                state.disable_keys.error =
                    Some("device connection changed while applying; read the device again".into());
            }
            DisableKeysPersistenceStatus::AwaitingReload(context)
            | DisableKeysPersistenceStatus::SavedNotReloaded(context) => {
                state.disable_keys.persistence =
                    DisableKeysPersistenceStatus::SavedNotReloadedDetached(
                        DisableKeysRecoveryToken {
                            key: context.key,
                            transaction_id: context.request_id,
                        },
                    );
            }
            DisableKeysPersistenceStatus::Idle
            | DisableKeysPersistenceStatus::AppliedNotSaved { .. }
            | DisableKeysPersistenceStatus::SavedNotReloadedDetached(_) => {}
        }
    }

    pub(crate) fn disable_keys_load_for(&self, key: &DeviceKey) -> DisableKeysLoad {
        self.disable_keys_reads.load(key)
    }

    pub(crate) fn disable_keys_status(
        &self,
        key: &DeviceKey,
    ) -> Option<&DisableKeysPersistenceStatus> {
        self.devices
            .runtime
            .get(key)
            .map(|state| &state.disable_keys.persistence)
    }

    pub(crate) fn disable_keys_error(&self, key: &DeviceKey) -> Option<&str> {
        self.devices
            .runtime
            .get(key)
            .and_then(|state| state.disable_keys.error.as_deref())
    }

    pub(crate) fn disable_keys_controls_enabled(&self, key: &DeviceKey) -> bool {
        self.config.is_writable()
            && self
                .devices
                .records
                .iter()
                .find(|record| record.device_key() == *key)
                .is_some_and(|record| record.online && record.is_persistent())
            && matches!(self.disable_keys_reads.load(key), Load::Ready(_))
            && matches!(
                self.disable_keys_status(key),
                None | Some(DisableKeysPersistenceStatus::Idle)
            )
    }
}

fn confirmed_known_set(state: DisableKeysState) -> BTreeSet<DisableKey> {
    DisableKey::ALL
        .into_iter()
        .filter(|key| {
            let bit = key.mask();
            state.supported.contains(bit) && state.disabled.contains(bit)
        })
        .collect()
}

fn transaction_fences_match(
    record_route: Option<&openlogi_core::hid::DeviceRoute>,
    active_generation: Option<u64>,
    active_request: Option<&DisableKeysRequestContext>,
    context: &DisableKeysRequestContext,
) -> bool {
    record_route == Some(&context.route)
        && active_generation == Some(context.route_generation)
        && active_request == Some(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::config::{Config, ConfigFile};
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };
    use openlogi_core::hid::{DIRECT_DEVICE_INDEX, DeviceRoute};

    use crate::services::assets::AssetResolver;
    use crate::state::ConfigPersistence;

    fn keyboard_inventory(product_id: u16, unit_id: [u8; 4]) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Keys".into(),
                vendor_id: 0x046d,
                product_id,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: DIRECT_DEVICE_INDEX,
                codename: Some("MX Keys".into()),
                wpid: None,
                kind: DeviceKind::Keyboard,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id,
                    transports: DeviceTransports::default(),
                    model_ids: [product_id, 0, 0],
                    extended_model_id: 0,
                }),
                capabilities: Some(Capabilities {
                    disable_keys: true,
                    ..Capabilities::default()
                }),
            }],
        }
    }

    fn test_state(
        persistence: ConfigPersistence,
        inventories: &[DeviceInventory],
    ) -> (
        AppState,
        tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
        DeviceKey,
    ) {
        let config = match &persistence {
            ConfigPersistence::UserFile(file) => file.reload().expect("tracked config").0,
            ConfigPersistence::ReadOnly(_) | ConfigPersistence::MemoryOnly => Config::ephemeral(),
        };
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            config,
            inventories,
            &[],
            &AssetResolver::new(),
            &[],
            persistence,
            commands,
        );
        while receiver.try_recv().is_ok() {}
        let key = state.current_record().expect("keyboard").device_key();
        state
            .disable_keys_reads
            .install_generation_for_test(key.clone(), 7);
        state.disable_keys_reads.set_confirmed(
            &key,
            DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND,
                disabled: DisableKeysMask::EMPTY,
            },
        );
        (state, receiver, key)
    }

    fn begin(
        state: &mut AppState,
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
    ) -> DisableKeysRequestContext {
        assert!(
            state
                .begin_disable_keys_write(DisableKeysMask::CAPS_LOCK)
                .is_some()
        );
        let Ok(Command::SetDisableKeys(context, desired)) = receiver.try_recv() else {
            panic!("expected guarded write");
        };
        assert_eq!(desired, DisableKeysMask::CAPS_LOCK);
        context
    }

    #[test]
    fn persisted_disable_keys_uses_only_confirmed_supported_known_bits() {
        let set = confirmed_known_set(DisableKeysState {
            supported: DisableKeysMask::from_bits_retain(0xb1),
            disabled: DisableKeysMask::from_bits_retain(0xb3),
        });
        assert_eq!(
            set,
            BTreeSet::from([DisableKey::CapsLock, DisableKey::WindowsCommand])
        );
    }

    #[test]
    fn every_disable_keys_acceptance_fence_is_required() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let context = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard"),
            route: route.clone(),
            route_generation: 7,
            request_id: 9,
        };
        assert!(transaction_fences_match(
            Some(&route),
            Some(7),
            Some(&context),
            &context
        ));
        assert!(!transaction_fences_match(
            None,
            Some(7),
            Some(&context),
            &context
        ));
        let other_route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35c,
        };
        assert!(!transaction_fences_match(
            Some(&other_route),
            Some(7),
            Some(&context),
            &context
        ));
        assert!(!transaction_fences_match(
            Some(&route),
            Some(8),
            Some(&context),
            &context
        ));
        let newer = DisableKeysRequestContext {
            request_id: 10,
            ..context.clone()
        };
        assert!(!transaction_fences_match(
            Some(&route),
            Some(7),
            Some(&newer),
            &context
        ));
    }

    #[test]
    fn hardware_failure_keeps_snapshot_and_config_unchanged() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = begin(&mut state, &mut receiver);

        assert!(state.apply_disable_keys_write_result(context, Err(WriteError::AgentUnavailable)));

        assert_eq!(
            state
                .disable_keys_reads
                .confirmed(&key)
                .expect("old snapshot")
                .disabled,
            DisableKeysMask::EMPTY
        );
        assert_eq!(state.config.disabled_keys(key.as_str()), None);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn save_failure_is_local_and_retry_merges_external_edit_without_second_write() {
        let temp = tempfile::tempdir().expect("temp config");
        let path = temp.path().join("config.toml");
        Config::default().save_to_path(&path).expect("seed config");
        let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::UserFile(file), &[inventory]);
        let context = begin(&mut state, &mut receiver);
        let mut external = std::fs::read_to_string(&path).expect("read config");
        external.push_str("\n# external edit retained\n");
        std::fs::write(&path, external).expect("external edit");

        assert!(state.apply_disable_keys_write_result(
            context,
            Ok(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND,
                disabled: DisableKeysMask::CAPS_LOCK,
            })
        ));
        assert_eq!(state.config.disabled_keys(key.as_str()), None);
        assert_eq!(state.config_issue(), None);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        assert!(receiver.try_recv().is_err());

        assert!(state.retry_disable_keys_save_inner(&key));
        let Ok(Command::ReloadConfig(ConfigReloadContext::DisableKeys(_))) = receiver.try_recv()
        else {
            panic!("save retry must send only contextual reload");
        };
        assert!(receiver.try_recv().is_err());
        assert!(
            std::fs::read_to_string(&path)
                .expect("saved config")
                .contains("# external edit retained")
        );
        assert_eq!(
            state.config.disabled_keys(key.as_str()),
            Some(&BTreeSet::from([DisableKey::CapsLock]))
        );
    }

    #[test]
    fn reload_failure_retry_never_reissues_hid_write() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = begin(&mut state, &mut receiver);
        assert!(state.apply_disable_keys_write_result(
            context,
            Ok(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK,
                disabled: DisableKeysMask::CAPS_LOCK,
            })
        ));
        let Ok(Command::ReloadConfig(ConfigReloadContext::DisableKeys(reload_context))) =
            receiver.try_recv()
        else {
            panic!("expected contextual reload");
        };
        assert!(state.apply_disable_keys_reload_result(
            reload_context,
            Err(openlogi_ipc::ConfigReloadError {
                message: "scripted".into()
            })
        ));
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded(_))
        ));

        assert!(state.retry_disable_keys_reload_inner(&key));
        assert!(matches!(
            receiver.try_recv(),
            Ok(Command::ReloadConfig(ConfigReloadContext::DisableKeys(_)))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn selection_change_does_not_invalidate_original_devices_result() {
        let first = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let second = keyboard_inventory(0xb35c, [5, 6, 7, 8]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[first, second]);
        let context = begin(&mut state, &mut receiver);
        assert!(state.devices.select(1));

        assert!(state.apply_disable_keys_write_result(
            context,
            Ok(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK,
                disabled: DisableKeysMask::CAPS_LOCK,
            })
        ));
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AwaitingReload(_))
        ));
    }

    #[test]
    fn invalidation_never_leaves_applying_or_awaiting_stuck() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = begin(&mut state, &mut receiver);
        state.invalidate_disable_keys(&key);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::Idle)
        ));

        state
            .disable_keys_reads
            .install_generation_for_test(key.clone(), context.route_generation + 1);
        state
            .devices
            .runtime
            .entry(key.clone())
            .or_default()
            .disable_keys
            .persistence = DisableKeysPersistenceStatus::AwaitingReload(context.clone());
        state.invalidate_disable_keys(&key);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloadedDetached(_))
        ));
    }
}

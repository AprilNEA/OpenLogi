//! Guarded Disable Keys reads, writes, persistence, and recovery.

use std::collections::BTreeSet;

use gpui::{App, Context};
use openlogi_core::config::DisableKey;
use openlogi_core::hid::{DisableKeysMask, DisableKeysState, WriteError};

use crate::services::ipc::{DisableKeysRequestContext, ReloadConfig, SetDisableKeys};

use super::{AppState, DeviceKey, DisableKeysLoad, Load, StateEvent, StateEvents};

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
    pub(crate) fn apply_disable_keys_read(
        &mut self,
        key: &DeviceKey,
        generation: u64,
        load: DisableKeysLoad,
    ) -> StateEvents {
        self.disable_keys_reads
            .update(key, generation, load)
            .then_some(StateEvent::DisableKeysChanged(key.clone()))
            .into()
    }

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
        let state = self.devices.sessions.entry(key.clone()).or_default();
        if !matches!(
            state.disable_keys.persistence,
            DisableKeysPersistenceStatus::Idle
        ) {
            return None;
        }
        state.disable_keys.persistence = DisableKeysPersistenceStatus::Applying(context.clone());
        state.disable_keys.error = None;
        if !self.send_ipc(SetDisableKeys { context, desired }) {
            let state = self.devices.sessions.entry(key.clone()).or_default();
            state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
            state.disable_keys.error = Some("agent is unavailable".into());
        }
        Some(key)
    }

    pub(crate) fn apply_disable_keys_write_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<DisableKeysState, WriteError>,
    ) -> StateEvents {
        let events = StateEvents::from(StateEvent::DisableKeysChanged(context.key.clone()));
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
            return StateEvents::none();
        }
        let key = context.key.clone();
        let result = match result {
            Ok(confirmed) => confirmed,
            Err(error) => {
                self.disable_keys_reads.reconcile_after_write(&key);
                let state = self.devices.sessions.entry(key).or_default();
                state.disable_keys.persistence = DisableKeysPersistenceStatus::Idle;
                state.disable_keys.error = Some(error.to_string());
                return events;
            }
        };

        self.disable_keys_reads.set_confirmed(&key, result);
        let known = confirmed_known_set(result);
        self.config
            .edit(|config| config.set_disabled_keys(key.as_str(), known));
        if let Err(error) = self.config.persist_feature("disabled keys") {
            let state = self.devices.sessions.entry(key.clone()).or_default();
            state.disable_keys.persistence = DisableKeysPersistenceStatus::AppliedNotSaved {
                recovery: DisableKeysRecoveryToken {
                    key,
                    transaction_id: context.request_id,
                },
                confirmed: result,
            };
            state.disable_keys.error = Some(error);
            return events;
        }

        self.request_disable_keys_reload(context);
        events
    }

    /// Enter the awaiting phase only while the IPC worker can carry the
    /// request. A stopped worker cannot ever produce a reload verdict.
    fn request_disable_keys_reload(&mut self, context: DisableKeysRequestContext) {
        let key = context.key.clone();
        let state = self.devices.sessions.entry(key.clone()).or_default();
        state.disable_keys.persistence =
            DisableKeysPersistenceStatus::AwaitingReload(context.clone());
        state.disable_keys.error = None;
        if !self.send_ipc(ReloadConfig::disable_keys(
            context.clone(),
            self.config.revision(),
        )) {
            let state = self.devices.sessions.entry(key).or_default();
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::SavedNotReloaded(context);
            state.disable_keys.error = Some("agent is unavailable".into());
        }
    }

    pub(crate) fn apply_disable_keys_reload_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<(), openlogi_ipc::ConfigReloadError>,
    ) -> StateEvents {
        let events = StateEvents::from(StateEvent::DisableKeysChanged(context.key.clone()));
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
            return StateEvents::none();
        }
        let state = self
            .devices
            .sessions
            .entry(context.key.clone())
            .or_default();
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
        events
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
                .sessions
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
                .sessions
                .entry(key.clone())
                .or_default()
                .disable_keys
                .error = Some(error);
            return true;
        }

        let next = self.allocate_disable_keys_context(key);
        if let Some(context) = next {
            self.request_disable_keys_reload(context);
        } else {
            let state = self.devices.sessions.entry(key.clone()).or_default();
            state.disable_keys.error = None;
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
            let state = self.devices.sessions.entry(key.clone()).or_default();
            state.disable_keys.persistence =
                DisableKeysPersistenceStatus::SavedNotReloadedDetached(recovery);
            return true;
        };
        self.request_disable_keys_reload(context);
        true
    }

    fn allocate_disable_keys_context(
        &mut self,
        key: &DeviceKey,
    ) -> Option<DisableKeysRequestContext> {
        let record = self.devices.record(key).filter(|record| record.online)?;
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
            .record(key)
            .and_then(|record| record.route.as_ref())
    }

    pub(crate) fn invalidate_disable_keys(&mut self, key: &DeviceKey) {
        self.disable_keys_reads.remove(key);
        let Some(state) = self.devices.sessions.get_mut(key) else {
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
            .sessions
            .get(key)
            .map(|state| &state.disable_keys.persistence)
    }

    pub(crate) fn disable_keys_error(&self, key: &DeviceKey) -> Option<&str> {
        self.devices
            .sessions
            .get(key)
            .and_then(|state| state.disable_keys.error.as_deref())
    }

    pub(crate) fn disable_keys_controls_enabled(&self, key: &DeviceKey) -> bool {
        self.config.is_writable()
            && self
                .devices
                .record(key)
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
    use gpui::AppContext as _;
    use openlogi_core::config::{Config, ConfigFile};
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };
    use openlogi_core::hid::{DIRECT_DEVICE_INDEX, DeviceRoute};

    use crate::services::assets::AssetResolver;
    use crate::services::ipc::{Command, ConfigReloadContext};
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
                settings_revision: 0,
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
        let mut state = AppState::new(super::super::Sources {
            config,
            inventories,
            standalone: &[],
            resolver: &AssetResolver::new(),
            cameras: &[],
            persistence,
            ipc_commands: commands,
        });
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

    #[test]
    fn catalog_lookup_is_independent_of_selection_and_handles_missing_keys() {
        let first = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let second = keyboard_inventory(0xb35c, [5, 6, 7, 8]);
        let (mut state, _, key) = test_state(ConfigPersistence::MemoryOnly, &[first, second]);
        let route = state.record_route(&key).cloned();
        assert!(route.is_some());
        assert!(state.devices.select(1));
        assert_eq!(state.record_route(&key), route.as_ref());
        assert!(state.devices.record(&DeviceKey::from("missing")).is_none());
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
        let Ok(Command::SetDisableKeys(SetDisableKeys { context, desired })) = receiver.try_recv()
        else {
            panic!("expected guarded write");
        };
        assert_eq!(desired, DisableKeysMask::CAPS_LOCK);
        context
    }

    fn take_disable_keys_reload(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
    ) -> DisableKeysRequestContext {
        let Ok(Command::ReloadConfig(ReloadConfig { mut contexts, .. })) = receiver.try_recv()
        else {
            panic!("expected contextual reload");
        };
        assert_eq!(contexts.len(), 1, "one transaction owns this reload");
        let ConfigReloadContext::DisableKeys(context) = contexts.remove(0) else {
            panic!("expected Disable Keys reload context");
        };
        context
    }

    fn install_reads(state: AppState, cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let runtime: std::sync::Arc<dyn swr_core::Runtime> =
                std::sync::Arc::new(swr_gpui::GpuiRuntime::new(cx));
            let client = swr_core::SwrClient::builder().build(runtime.clone());
            let state = cx.new(|_| {
                let mut state = state;
                state.connect_device_reads(client, runtime);
                state
            });
            AppState::set_global(state, cx);
            AppState::update(cx, AppState::load_current_disable_keys);
        });
        cx.run_until_parked();
    }

    fn take_read(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> crate::services::ipc::ReadDisableKeys {
        let Ok(Command::ReadDisableKeys(read)) = receiver.try_recv() else {
            panic!("one Disable Keys read, not a write or config reload");
        };
        assert!(receiver.try_recv().is_err(), "no extra device commands");
        read
    }

    fn refresh_inventory(
        inventory: &DeviceInventory,
        cx: &mut gpui::TestAppContext,
    ) -> StateEvents {
        let events = cx.update(|cx| {
            AppState::update(cx, |state, _| {
                state.refresh_inventories(
                    std::slice::from_ref(inventory),
                    &[],
                    &AssetResolver::new(),
                    &[],
                )
            })
        });
        cx.run_until_parked();
        events
    }

    #[gpui::test]
    fn a_settings_revision_discards_an_older_disable_keys_read(cx: &mut gpui::TestAppContext) {
        let mut inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            std::slice::from_ref(&inventory),
        );
        install_reads(state, cx);
        let first = take_read(&mut receiver);
        inventory.paired[0].settings_revision = 1;
        assert_eq!(
            refresh_inventory(&inventory, cx),
            [StateEvent::InventoryChanged]
        );
        let current = take_read(&mut receiver);
        assert_eq!(current.route, first.route);
        let confirmed = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::CAPS_LOCK,
        };
        let _ = current.reply.send(Ok(confirmed));
        cx.run_until_parked();
        let _ = first.reply.send(Ok(DisableKeysState::default()));
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).expect("state installed");
            assert_eq!(
                state.disable_keys_load_for(&key),
                Load::Ready(std::sync::Arc::new(confirmed))
            );
            assert!(
                state.config.disabled_keys(key.as_str()).is_none(),
                "a report never persists a policy"
            );
        });
        assert!(refresh_inventory(&inventory, cx).is_empty());
        assert!(
            receiver.try_recv().is_err(),
            "an unchanged snapshot does not re-read"
        );
    }

    #[gpui::test]
    fn a_settings_revision_does_not_retire_a_pending_disable_keys_write(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            std::slice::from_ref(&inventory),
        );
        install_reads(state, cx);
        let confirmed = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::CAPS_LOCK,
        };
        let _ = take_read(&mut receiver).reply.send(Ok(confirmed));
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));
        inventory.paired[0].settings_revision = 1;
        let _ = refresh_inventory(&inventory, cx);
        let _ = take_read(&mut receiver).reply.send(Ok(confirmed));
        cx.run_until_parked();
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                assert_eq!(
                    state.disable_keys_status(&key),
                    Some(&DisableKeysPersistenceStatus::Applying(context.clone()))
                );
                assert_eq!(
                    state.apply_disable_keys_write_result(context.clone(), Ok(confirmed)),
                    [StateEvent::DisableKeysChanged(key.clone())]
                );
                assert_eq!(
                    state.disable_keys_status(&key),
                    Some(&DisableKeysPersistenceStatus::AwaitingReload(
                        context.clone()
                    ))
                );
            });
        });
        assert_eq!(take_disable_keys_reload(&mut receiver), context);
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
    fn hardware_failure_retires_snapshot_without_changing_config() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = begin(&mut state, &mut receiver);

        assert_eq!(
            state.apply_disable_keys_write_result(context, Err(WriteError::AgentUnavailable)),
            [StateEvent::DisableKeysChanged(key.clone())]
        );

        assert!(state.disable_keys_reads.confirmed(&key).is_none());
        assert!(!state.disable_keys_controls_enabled(&key));
        assert!(
            state
                .begin_disable_keys_write(DisableKeysMask::EMPTY)
                .is_none()
        );
        assert_eq!(state.config.disabled_keys(key.as_str()), None);
        assert!(receiver.try_recv().is_err());
    }

    #[gpui::test]
    fn unconfirmed_disable_keys_write_requires_fresh_read_before_another_toggle(
        cx: &mut gpui::TestAppContext,
    ) {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) = test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        install_reads(state, cx);
        let original = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver)
            .reply
            .send(Ok(original))
            .expect("initial read");
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));

        // An unsolicited read was already in flight when the write changed
        // hardware. Neither this result nor the pre-write cache is authoritative.
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                state.disable_keys_reads.revalidate_device_settings(&key);
            });
        });
        cx.run_until_parked();
        let stale_read = take_read(&mut receiver);
        let hardware = DisableKeysState {
            disabled: DisableKeysMask::CAPS_LOCK,
            ..original
        };
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                let _ = state.apply_disable_keys_write_result(
                    context,
                    Err(WriteError::RequestTimedOut {
                        operation: openlogi_core::hid::HidppOperation::WriteDisableKeys,
                    }),
                );
                assert!(!state.disable_keys_controls_enabled(&key));
                assert!(
                    state
                        .begin_disable_keys_write(DisableKeysMask::EMPTY)
                        .is_none()
                );
                assert_eq!(state.config.disabled_keys(key.as_str()), None);
            });
        });
        cx.run_until_parked();
        let fresh_read = take_read(&mut receiver);
        let _ = stale_read.reply.send(Ok(original));
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                !AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });
        fresh_read.reply.send(Ok(hardware)).expect("fresh read");
        cx.run_until_parked();
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                assert_eq!(
                    state.disable_keys_load_for(&key),
                    Load::Ready(std::sync::Arc::new(hardware))
                );
                assert!(state.disable_keys_controls_enabled(&key));
                assert_eq!(
                    state.config.disabled_keys(key.as_str()),
                    None,
                    "rereading never saves the request"
                );
                assert!(
                    state
                        .begin_disable_keys_write(DisableKeysMask::EMPTY)
                        .is_some()
                );
            });
        });
        let Command::SetDisableKeys(SetDisableKeys { desired, .. }) =
            receiver.try_recv().expect("undo")
        else {
            panic!("fresh state permits undo, not reload");
        };
        assert_eq!(desired, DisableKeysMask::EMPTY);
        assert!(receiver.try_recv().is_err());
    }

    #[gpui::test]
    fn same_route_reconnect_fences_an_outstanding_disable_keys_reconciliation(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            std::slice::from_ref(&inventory),
        );
        install_reads(state, cx);
        let original = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver)
            .reply
            .send(Ok(original))
            .expect("initial read");
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                let _ = state.apply_disable_keys_write_result(
                    context.clone(),
                    Err(WriteError::AgentUnavailable),
                );
            });
        });
        cx.run_until_parked();
        let old_read = take_read(&mut receiver);

        inventory.paired[0].online = false;
        let _ = refresh_inventory(&inventory, cx);
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.disable_keys_controls_enabled(&key));
            assert!(state.disable_keys_reads.generation(&key).is_none());
        });
        inventory.paired[0].online = true;
        let _ = refresh_inventory(&inventory, cx);
        cx.update(|cx| AppState::update(cx, AppState::load_current_disable_keys));
        cx.run_until_parked();
        let fresh_read = take_read(&mut receiver);
        assert_eq!(
            fresh_read.route, old_read.route,
            "route identity is unchanged"
        );
        let _ = old_read.reply.send(Ok(original));
        cx.run_until_parked();
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                assert_ne!(
                    state.disable_keys_reads.generation(&key),
                    Some(context.route_generation),
                    "the new connection owns a new subscription generation"
                );
                assert!(!state.disable_keys_controls_enabled(&key));
                assert!(
                    state
                        .begin_disable_keys_write(DisableKeysMask::EMPTY)
                        .is_none()
                );
                assert_eq!(state.config.disabled_keys(key.as_str()), None);
            });
        });
        let current = DisableKeysState {
            disabled: DisableKeysMask::CAPS_LOCK,
            ..original
        };
        fresh_read
            .reply
            .send(Ok(current))
            .expect("current connection read");
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert_eq!(
                state.disable_keys_load_for(&key),
                Load::Ready(std::sync::Arc::new(current))
            );
            assert!(state.disable_keys_controls_enabled(&key));
            assert_eq!(state.config.disabled_keys(key.as_str()), None);
        });
        assert!(
            receiver.try_recv().is_err(),
            "neither completion writes or reloads config"
        );
    }

    #[gpui::test]
    fn failed_disable_keys_reconciliation_stays_blocked_until_retry_succeeds(
        cx: &mut gpui::TestAppContext,
    ) {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) = test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        install_reads(state, cx);
        let hardware = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver)
            .reply
            .send(Ok(hardware))
            .expect("initial read");
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                let _ = state
                    .apply_disable_keys_write_result(context, Err(WriteError::AgentUnavailable));
                assert!(!state.disable_keys_controls_enabled(&key));
            });
        });
        cx.run_until_parked();
        for _ in 0..3 {
            take_read(&mut receiver)
                .reply
                .send(Err(WriteError::AgentUnavailable))
                .expect("read failure");
            cx.run_until_parked();
        }
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(matches!(state.disable_keys_load_for(&key), Load::Failed(_)));
            assert!(!state.disable_keys_controls_enabled(&key));
            assert_eq!(state.config.disabled_keys(key.as_str()), None);
        });
        cx.update(|cx| AppState::retry_disable_keys_read(cx, key.clone()));
        cx.run_until_parked();
        take_read(&mut receiver)
            .reply
            .send(Ok(hardware))
            .expect("retry read");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn save_failure_is_local_and_retry_merges_external_edit_without_second_write() {
        let temp = tempfile::tempdir().expect("temp config");
        let path = temp.path().join(openlogi_core::paths::CONFIG_FILE);
        Config::default().save_to_path(&path).expect("seed config");
        let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::UserFile(file), &[inventory]);
        let context = begin(&mut state, &mut receiver);
        let mut external = std::fs::read_to_string(&path).expect("read config");
        external.push_str("\n# external edit retained\n");
        std::fs::write(&path, external).expect("external edit");

        assert_eq!(
            state.apply_disable_keys_write_result(
                context,
                Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND,
                    disabled: DisableKeysMask::CAPS_LOCK,
                })
            ),
            [StateEvent::DisableKeysChanged(key.clone())]
        );
        assert_eq!(state.config.disabled_keys(key.as_str()), None);
        assert_eq!(state.config_issue(), None);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        assert!(receiver.try_recv().is_err());

        assert!(state.retry_disable_keys_save_inner(&key));
        take_disable_keys_reload(&mut receiver);
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
        assert_eq!(
            state.apply_disable_keys_write_result(
                context,
                Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK,
                    disabled: DisableKeysMask::CAPS_LOCK,
                })
            ),
            [StateEvent::DisableKeysChanged(key.clone())]
        );
        let reload_context = take_disable_keys_reload(&mut receiver);
        assert_eq!(
            state.apply_disable_keys_reload_result(
                reload_context,
                Err(openlogi_ipc::ConfigReloadError {
                    message: "scripted".into()
                })
            ),
            [StateEvent::DisableKeysChanged(key.clone())]
        );
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded(_))
        ));

        assert!(state.retry_disable_keys_reload_inner(&key));
        take_disable_keys_reload(&mut receiver);
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

        assert_eq!(
            state.apply_disable_keys_write_result(
                context,
                Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK,
                    disabled: DisableKeysMask::CAPS_LOCK,
                })
            ),
            [StateEvent::DisableKeysChanged(key.clone())]
        );
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AwaitingReload(_))
        ));
    }

    #[test]
    fn disable_keys_reload_retry_with_closed_ipc_retains_recovery() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = begin(&mut state, &mut receiver);
        let _ = state.apply_disable_keys_write_result(
            context,
            Ok(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK,
                disabled: DisableKeysMask::CAPS_LOCK,
            }),
        );
        let context = take_disable_keys_reload(&mut receiver);
        let _ = state.apply_disable_keys_reload_result(
            context,
            Err(openlogi_ipc::ConfigReloadError {
                message: "scripted reload failure".into(),
            }),
        );
        drop(receiver);

        assert!(state.retry_disable_keys_reload_inner(&key));
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded(_))
        ));
        assert_eq!(state.disable_keys_error(&key), Some("agent is unavailable"));
    }

    #[test]
    fn disable_keys_save_retry_with_closed_ipc_retains_reload_recovery() {
        let temp = tempfile::tempdir().expect("temp config");
        let path = temp.path().join(openlogi_core::paths::CONFIG_FILE);
        Config::default().save_to_path(&path).expect("seed config");
        let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::UserFile(file), &[inventory]);
        let context = begin(&mut state, &mut receiver);
        let external = format!(
            "{}\n# external edit\n",
            std::fs::read_to_string(&path).expect("read config")
        );
        std::fs::write(&path, external).expect("external edit");
        let _ = state.apply_disable_keys_write_result(
            context,
            Ok(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK,
                disabled: DisableKeysMask::CAPS_LOCK,
            }),
        );
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        drop(receiver);

        assert!(state.retry_disable_keys_save_inner(&key));
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded(_))
        ));
        assert_eq!(state.disable_keys_error(&key), Some("agent is unavailable"));
        assert_eq!(
            ConfigFile::load_from_path(&path)
                .expect("saved config")
                .0
                .disabled_keys(key.as_str()),
            Some(&BTreeSet::from([DisableKey::CapsLock]))
        );
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
            .sessions
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

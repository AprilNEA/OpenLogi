//! Guarded Disable Keys reads, writes, persistence, and recovery.

use std::collections::BTreeSet;

use gpui::{App, Context};
use openlogi_core::config::DisableKey;
use openlogi_core::hid::{DeviceRoute, DisableKeysMask, DisableKeysState, WriteError};
use tracing::warn;

use crate::services::ipc::{
    ConfigReloadContext, DisableKeysRequestContext, ReloadConfig, SetDisableKeys,
};

use super::{
    AppState, ConfigReconciliation, DeviceKey, DeviceRecord, DisableKeysLoad, Load, StateEvent,
    StateEvents,
};

/// Route-independent identity retained across disconnects during recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisableKeysRecoveryToken {
    pub(crate) key: DeviceKey,
    pub(crate) transaction_id: u64,
}

impl From<DisableKeysRequestContext> for DisableKeysRecoveryToken {
    fn from(context: DisableKeysRequestContext) -> Self {
        Self {
            key: context.key,
            transaction_id: context.request_id,
        }
    }
}

/// Persistence phase for one device's confirmed Disable Keys transaction.
///
/// A recoverable failure travels inside the phase that reports it. The phases
/// that wait on the agent have no room for one, and a recovery phase keeps
/// the failure that led to it until a retry replaces it. Only the transitions
/// below change a phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DisableKeysPersistenceStatus {
    /// No transaction is outstanding. `error` says how the last one ended,
    /// until the next write starts or a reload is accepted.
    Idle {
        error: Option<DisableKeysError>,
    },
    Applying(DisableKeysRequestContext),
    AppliedNotSaved {
        recovery: DisableKeysRecoveryToken,
        confirmed: DisableKeysState,
        error: DisableKeysError,
    },
    AwaitingReload(DisableKeysRequestContext),
    SavedNotReloaded {
        context: DisableKeysRequestContext,
        error: DisableKeysError,
    },
    SavedNotReloadedDetached {
        recovery: DisableKeysRecoveryToken,
        error: Option<DisableKeysError>,
    },
    /// The agent loaded another revision, but the GUI could not refresh it.
    ReconcileRequired {
        recovery: DisableKeysRecoveryToken,
        error: Option<DisableKeysError>,
    },
    /// A refreshed external file needs an agent receipt and a subsequent read.
    AwaitingReconciliationReload(DisableKeysRequestContext),
}

impl Default for DisableKeysPersistenceStatus {
    fn default() -> Self {
        Self::Idle { error: None }
    }
}

impl DisableKeysPersistenceStatus {
    /// The failure this phase reports, if any.
    fn error(&self) -> Option<DisableKeysError> {
        match self {
            Self::Idle { error }
            | Self::SavedNotReloadedDetached { error, .. }
            | Self::ReconcileRequired { error, .. } => *error,
            Self::AppliedNotSaved { error, .. } | Self::SavedNotReloaded { error, .. } => {
                Some(*error)
            }
            Self::Applying(_) | Self::AwaitingReload(_) | Self::AwaitingReconciliationReload(_) => {
                None
            }
        }
    }

    /// Whether `receipt` answers the write this phase is waiting on.
    fn awaits_write(&self, fence: RouteFence<'_>, receipt: &DisableKeysRequestContext) -> bool {
        matches!(self, Self::Applying(active) if fence.admits(active, receipt))
    }

    /// Why the reload `receipt` answers was sent, when this phase waits on it.
    fn awaited_reload(
        &self,
        fence: RouteFence<'_>,
        receipt: &DisableKeysRequestContext,
    ) -> Option<ReloadPurpose> {
        match self {
            Self::AwaitingReload(active) if fence.admits(active, receipt) => {
                Some(ReloadPurpose::Saved)
            }
            Self::AwaitingReconciliationReload(active) if fence.admits(active, receipt) => {
                Some(ReloadPurpose::Reconciled)
            }
            _ => None,
        }
    }

    /// The agent has the write.
    fn write_sent(&mut self, context: DisableKeysRequestContext) {
        *self = Self::Applying(context);
    }

    /// The write ended without a confirmed device state.
    fn write_failed(&mut self, error: DisableKeysError) {
        *self = Self::Idle { error: Some(error) };
    }

    /// The device holds `confirmed`, but the config file does not.
    fn save_failed(
        &mut self,
        recovery: DisableKeysRecoveryToken,
        confirmed: DisableKeysState,
        error: DisableKeysError,
    ) {
        *self = Self::AppliedNotSaved {
            recovery,
            confirmed,
            error,
        };
    }

    /// The saved file is on its way to the agent.
    fn reload_sent(&mut self, context: DisableKeysRequestContext, purpose: ReloadPurpose) {
        *self = match purpose {
            ReloadPurpose::Saved => Self::AwaitingReload(context),
            ReloadPurpose::Reconciled => Self::AwaitingReconciliationReload(context),
        };
    }

    /// The agent did not adopt the saved file: the request never left, or
    /// the agent refused it.
    fn reload_failed(
        &mut self,
        context: DisableKeysRequestContext,
        purpose: ReloadPurpose,
        error: DisableKeysError,
    ) {
        *self = match purpose {
            ReloadPurpose::Saved => Self::SavedNotReloaded { context, error },
            ReloadPurpose::Reconciled => Self::ReconcileRequired {
                recovery: context.into(),
                error: Some(error),
            },
        };
    }

    /// The agent adopted the exact revision the transaction saved.
    fn reload_accepted(&mut self) {
        *self = Self::default();
    }

    /// The file is saved, but no connection can carry its reload now.
    fn reload_detached(
        &mut self,
        recovery: DisableKeysRecoveryToken,
        error: Option<DisableKeysError>,
    ) {
        *self = Self::SavedNotReloadedDetached { recovery, error };
    }

    /// The externally changed file still has to be refreshed and confirmed.
    fn reconcile_required(
        &mut self,
        recovery: DisableKeysRecoveryToken,
        error: Option<DisableKeysError>,
    ) {
        *self = Self::ReconcileRequired { recovery, error };
    }

    /// The connection an in-flight request went out on is gone. A write can
    /// no longer be confirmed; a saved file still needs its reload.
    fn connection_changed(&mut self) {
        *self = match std::mem::take(self) {
            Self::Applying(_) => Self::Idle {
                error: Some(DisableKeysError::ConnectionChanged),
            },
            Self::AwaitingReload(context) => Self::SavedNotReloadedDetached {
                recovery: context.into(),
                error: None,
            },
            Self::SavedNotReloaded { context, error } => Self::SavedNotReloadedDetached {
                recovery: context.into(),
                error: Some(error),
            },
            Self::AwaitingReconciliationReload(context) => Self::ReconcileRequired {
                recovery: context.into(),
                error: None,
            },
            settled @ (Self::Idle { .. }
            | Self::AppliedNotSaved { .. }
            | Self::SavedNotReloadedDetached { .. }
            | Self::ReconcileRequired { .. }) => settled,
        };
    }
}

/// Why a saved file goes back to the agent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReloadPurpose {
    /// This transaction saved the confirmed keys.
    Saved,
    /// This transaction refreshed an externally changed file.
    Reconciled,
}

/// The connection a receipt must still be on: the device's current route and
/// the generation of its Disable Keys read subscription.
#[derive(Clone, Copy)]
struct RouteFence<'a> {
    route: Option<&'a DeviceRoute>,
    route_generation: Option<u64>,
}

impl RouteFence<'_> {
    /// `receipt` answers `active`, the request a phase waits on, over the
    /// connection that request went out on.
    fn admits(
        self,
        active: &DisableKeysRequestContext,
        receipt: &DisableKeysRequestContext,
    ) -> bool {
        self.route == Some(&receipt.route)
            && self.route_generation == Some(receipt.route_generation)
            && active == receipt
    }
}

/// Every fact the Disable Keys switches depend on, derived in
/// [`AppState::disable_keys_controls_enabled`] and nowhere else.
///
/// This is where a device's own phase meets the shared
/// [`ConfigReconciliation`]: a device writes only while its transaction is
/// idle *and* no adopted file waits for an agent receipt. A receipt pending
/// for any device therefore closes every device's switches.
#[derive(Clone, Copy)]
struct WriteGate<'a> {
    reconciliation: &'a ConfigReconciliation,
    config_writable: bool,
    online: bool,
    persistent: bool,
    load: &'a DisableKeysLoad,
    phase: Option<&'a DisableKeysPersistenceStatus>,
}

impl WriteGate<'_> {
    fn is_open(self) -> bool {
        *self.reconciliation == ConfigReconciliation::Confirmed
            && self.config_writable
            && self.online
            && self.persistent
            && matches!(self.load, Load::Ready(_))
            && self
                .phase
                .is_none_or(|phase| matches!(phase, DisableKeysPersistenceStatus::Idle { .. }))
    }
}

/// Recoverable failure category; the panel localizes it at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisableKeysError {
    AgentUnavailable,
    ConnectionChanged,
    WriteFailed,
    SaveFailed,
    RefreshFailed,
    ReloadFailed,
}

/// Per-device Disable Keys transaction state. The field stays private to this
/// module so its transitions are the only writer.
#[derive(Debug, Default)]
pub(super) struct DisableKeysDeviceState {
    persistence: DisableKeysPersistenceStatus,
}

impl AppState {
    pub(crate) fn apply_disable_keys_read(
        &mut self,
        key: &DeviceKey,
        generation: u64,
        load: DisableKeysLoad,
    ) -> StateEvents {
        if !self.disable_keys_reads.update(key, generation, load) {
            return StateEvents::none();
        }
        if let Load::Failed(error) | Load::Unsupported(error) = self.disable_keys_reads.load(key) {
            warn!(%error, %key, "could not read disabled keys");
        }
        StateEvent::DisableKeysChanged(key.clone()).into()
    }

    pub(super) fn load_current_disable_keys(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self.current_record().and_then(|record| {
            let supported = record.capabilities.unwrap_or_default().disable_keys;
            let route = record.addressable_route().filter(|_| supported)?;
            Some((record.device_key(), route.clone()))
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

    pub(crate) fn retry_disable_keys_save(cx: &mut App, key: &DeviceKey) {
        Self::apply(cx, |state| state.retry_disable_keys_save_inner(key));
    }

    pub(crate) fn retry_disable_keys_reload(cx: &mut App, key: &DeviceKey) {
        Self::apply(cx, |state| state.retry_disable_keys_reload_inner(key));
    }

    fn begin_disable_keys_write(&mut self, desired: DisableKeysMask) -> Option<DeviceKey> {
        let key = self.current_record()?.device_key();
        if !self.disable_keys_controls_enabled(&key) {
            return None;
        }
        let context = self.allocate_disable_keys_context(&key)?;
        if self.send_ipc(SetDisableKeys {
            context: context.clone(),
            desired,
        }) {
            self.disable_keys_phase(&key).write_sent(context);
        } else {
            self.disable_keys_phase(&key)
                .write_failed(DisableKeysError::AgentUnavailable);
        }
        Some(key)
    }

    pub(crate) fn apply_disable_keys_write_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<DisableKeysState, WriteError>,
    ) -> StateEvents {
        let key = context.key.clone();
        let fence = self.disable_keys_fence(&key);
        if !self
            .disable_keys_status(&key)
            .is_some_and(|phase| phase.awaits_write(fence, &context))
        {
            return StateEvents::none();
        }
        let events = StateEvents::from(StateEvent::DisableKeysChanged(key.clone()));
        let confirmed = match result {
            Ok(confirmed) => confirmed,
            Err(error) => {
                warn!(%error, %key, "disabled keys write was not confirmed");
                self.disable_keys_reads.reconcile_after_write(&key);
                self.disable_keys_phase(&key)
                    .write_failed(DisableKeysError::WriteFailed);
                return events;
            }
        };

        self.disable_keys_reads.set_confirmed(&key, confirmed);
        let known = confirmed_known_set(confirmed);
        self.config
            .edit(|config| config.set_disabled_keys(key.as_str(), known));
        if let Err(error) = self.config.persist_feature("disabled keys") {
            warn!(%error, %key, "could not save disabled keys");
            self.disable_keys_phase(&key).save_failed(
                context.into(),
                confirmed,
                DisableKeysError::SaveFailed,
            );
            return events;
        }

        self.request_disable_keys_reload(context, ReloadPurpose::Saved);
        events
    }

    /// Enter the awaiting phase only while the IPC worker can carry the
    /// request. A stopped worker cannot ever produce a reload verdict.
    fn request_disable_keys_reload(
        &mut self,
        context: DisableKeysRequestContext,
        purpose: ReloadPurpose,
    ) {
        let key = context.key.clone();
        let reload_context = ConfigReloadContext::DisableKeys(context.clone());
        self.disable_keys_reads
            .track_config_reload(reload_context.clone());
        if self.send_ipc(ReloadConfig::disable_keys(
            context.clone(),
            self.config.revision(),
        )) {
            self.disable_keys_phase(&key).reload_sent(context, purpose);
        } else {
            self.disable_keys_reads
                .fail_config_reload(&reload_context, DisableKeysError::AgentUnavailable);
            self.disable_keys_phase(&key).reload_failed(
                context,
                purpose,
                DisableKeysError::AgentUnavailable,
            );
        }
    }

    pub(crate) fn apply_disable_keys_reload_result(
        &mut self,
        context: DisableKeysRequestContext,
        result: Result<openlogi_ipc::SavedConfigVerdict, openlogi_ipc::ConfigReloadError>,
    ) -> StateEvents {
        let key = context.key.clone();
        let fence = self.disable_keys_fence(&key);
        let Some(purpose) = self
            .disable_keys_status(&key)
            .and_then(|phase| phase.awaited_reload(fence, &context))
        else {
            return StateEvents::none();
        };
        let mut events = StateEvents::from(StateEvent::DisableKeysChanged(key.clone()));
        let reload_context = ConfigReloadContext::DisableKeys(context.clone());
        match result {
            Ok(openlogi_ipc::SavedConfigVerdict::Accepted) => {
                if self
                    .disable_keys_reads
                    .config_reload_matches(&reload_context)
                {
                    events = events.and(self.confirm_external_config_reload(&reload_context));
                } else if purpose == ReloadPurpose::Reconciled {
                    // A read started while refreshing the local file may have
                    // preceded the agent reload. Only a post-receipt read is current.
                    self.disable_keys_reads.reconcile_after_write(&key);
                }
                self.disable_keys_phase(&key).reload_accepted();
            }
            Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave) => {
                let _ = self.config.apply_reload_result(Ok(()));
                return self.reconcile_disable_keys_config(context.into(), None);
            }
            Err(error) => {
                warn!(error = %error.message, %key, "could not reload disabled keys");
                self.disable_keys_reads
                    .fail_config_reload(&reload_context, DisableKeysError::ReloadFailed);
                events = events.and(self.disable_keys_config_events());
                self.disable_keys_phase(&key).reload_failed(
                    context,
                    purpose,
                    DisableKeysError::ReloadFailed,
                );
            }
        }
        events
    }

    fn retry_disable_keys_save_inner(&mut self, key: &DeviceKey) -> StateEvents {
        let Some(DisableKeysPersistenceStatus::AppliedNotSaved {
            recovery,
            confirmed,
            ..
        }) = self.disable_keys_status(key).cloned()
        else {
            return StateEvents::none();
        };
        let events = StateEvents::from(StateEvent::DisableKeysChanged(key.clone()));
        let config_events = match self.reconcile_external_config() {
            Ok(events) => events,
            Err(config_events) => {
                self.disable_keys_phase(key).save_failed(
                    recovery,
                    confirmed,
                    DisableKeysError::RefreshFailed,
                );
                return events.and(config_events);
            }
        };
        let events = events.and(config_events);
        let known = confirmed_known_set(confirmed);
        self.config
            .edit(|config| config.set_disabled_keys(recovery.key.as_str(), known));
        if let Err(error) = self.config.persist_feature("disabled keys recovery") {
            warn!(%error, %key, "could not save disabled keys recovery");
            self.disable_keys_phase(key).save_failed(
                recovery,
                confirmed,
                DisableKeysError::SaveFailed,
            );
            return events;
        }

        match self.allocate_disable_keys_context(key) {
            Some(context) => self.request_disable_keys_reload(context, ReloadPurpose::Saved),
            None => self.disable_keys_phase(key).reload_detached(recovery, None),
        }
        events
    }

    fn retry_disable_keys_reload_inner(&mut self, key: &DeviceKey) -> StateEvents {
        let (recovery, error) = match self.disable_keys_status(key) {
            Some(DisableKeysPersistenceStatus::SavedNotReloaded { context, error }) => {
                (context.clone().into(), Some(*error))
            }
            Some(DisableKeysPersistenceStatus::SavedNotReloadedDetached { recovery, error }) => {
                (recovery.clone(), *error)
            }
            Some(DisableKeysPersistenceStatus::ReconcileRequired { recovery, error }) => {
                return self.reconcile_disable_keys_config(recovery.clone(), *error);
            }
            None | Some(DisableKeysPersistenceStatus::Idle { .. })
                if matches!(
                    self.disable_keys_config_reconciliation(),
                    ConfigReconciliation::Required(_)
                ) =>
            {
                return self.reconcile_saved_config();
            }
            _ => return StateEvents::none(),
        };
        let events = StateEvents::from(StateEvent::DisableKeysChanged(key.clone()));
        let Some(context) = self.allocate_disable_keys_context(&recovery.key) else {
            self.disable_keys_phase(key)
                .reload_detached(recovery, error);
            return events;
        };
        self.request_disable_keys_reload(context, ReloadPurpose::Saved);
        events
    }

    /// A mismatch identifies neither the file's present revision nor the one
    /// the GUI will refresh. Confirm that refreshed revision before accepting
    /// a subsequent hardware read; never persist the superseded choice here.
    /// `error` is the failure the device already reports; it stays when no
    /// connection can carry the reload.
    fn reconcile_disable_keys_config(
        &mut self,
        recovery: DisableKeysRecoveryToken,
        error: Option<DisableKeysError>,
    ) -> StateEvents {
        let key = recovery.key.clone();
        let events = StateEvents::from(StateEvent::DisableKeysChanged(key.clone()));
        let config_events = match self.reconcile_external_config() {
            Ok(events) => events,
            Err(config_events) => {
                self.disable_keys_phase(&key)
                    .reconcile_required(recovery, Some(DisableKeysError::RefreshFailed));
                return events.and(config_events);
            }
        };
        let events = events.and(config_events);
        let Some(context) = self.allocate_disable_keys_context(&key) else {
            self.disable_keys_phase(&key)
                .reconcile_required(recovery, error);
            return events;
        };
        self.request_disable_keys_reload(context, ReloadPurpose::Reconciled);
        events
    }

    fn allocate_disable_keys_context(
        &mut self,
        key: &DeviceKey,
    ) -> Option<DisableKeysRequestContext> {
        let route = self.devices.record(key)?.addressable_route()?.clone();
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

    fn record_route(&self, key: &DeviceKey) -> Option<&DeviceRoute> {
        self.devices
            .record(key)
            .and_then(|record| record.route.as_ref())
    }

    fn disable_keys_fence(&self, key: &DeviceKey) -> RouteFence<'_> {
        RouteFence {
            route: self.record_route(key),
            route_generation: self.disable_keys_reads.generation(key),
        }
    }

    /// The device's phase, for its transitions to advance.
    fn disable_keys_phase(&mut self, key: &DeviceKey) -> &mut DisableKeysPersistenceStatus {
        &mut self
            .devices
            .sessions
            .entry(key.clone())
            .or_default()
            .disable_keys
            .persistence
    }

    pub(crate) fn invalidate_disable_keys(&mut self, key: &DeviceKey) {
        self.disable_keys_reads.remove(key);
        if let Some(session) = self.devices.sessions.get_mut(key) {
            session.disable_keys.persistence.connection_changed();
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
            .map(|session| &session.disable_keys.persistence)
    }

    pub(crate) fn disable_keys_error(&self, key: &DeviceKey) -> Option<DisableKeysError> {
        self.disable_keys_status(key)
            .and_then(DisableKeysPersistenceStatus::error)
    }

    pub(crate) fn disable_keys_controls_enabled(&self, key: &DeviceKey) -> bool {
        let record = self.devices.record(key);
        WriteGate {
            reconciliation: self.disable_keys_config_reconciliation(),
            config_writable: self.config.is_writable(),
            online: record.is_some_and(DeviceRecord::is_online),
            persistent: record.is_some_and(DeviceRecord::is_persistent),
            load: &self.disable_keys_reads.load(key),
            phase: self.disable_keys_status(key),
        }
        .is_open()
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    use openlogi_core::config::{Config, ConfigFile};
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };
    use openlogi_core::hid::DIRECT_DEVICE_INDEX;

    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
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
                hidpp10: false,
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
        let ConfigReloadContext::DisableKeys(context) = contexts.remove(0).context() else {
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

    fn take_reconciliation_commands(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> (ReloadConfig, crate::services::ipc::ReadDisableKeys) {
        let mut reload = None;
        let mut read = None;
        while let Ok(command) = receiver.try_recv() {
            match command {
                Command::ReloadConfig(request) => {
                    assert!(reload.replace(request).is_none(), "one contextual reload");
                }
                Command::ReadDisableKeys(request) => {
                    assert!(read.replace(request).is_none(), "one refresh read");
                }
                _ => panic!("reconciliation must not submit manual hardware writes"),
            }
        }
        (
            reload.expect("revision confirmation"),
            read.expect("refresh read"),
        )
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
        let fence = |route, route_generation| RouteFence {
            route,
            route_generation,
        };
        assert!(fence(Some(&route), Some(7)).admits(&context, &context));
        assert!(!fence(None, Some(7)).admits(&context, &context));
        let other_route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35c,
        };
        assert!(!fence(Some(&other_route), Some(7)).admits(&context, &context));
        assert!(!fence(Some(&route), Some(8)).admits(&context, &context));
        assert!(!fence(Some(&route), None).admits(&context, &context));
        let newer = DisableKeysRequestContext {
            request_id: 10,
            ..context.clone()
        };
        assert!(!fence(Some(&route), Some(7)).admits(&newer, &context));
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
    fn settings_revision_invalidation_closes_the_ready_write_gate(cx: &mut gpui::TestAppContext) {
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
        cx.read(|cx| {
            let state = AppState::try_read(cx).expect("state");
            assert!(state.disable_keys_controls_enabled(&key));
        });

        inventory.paired[0].settings_revision = 1;
        assert_eq!(
            refresh_inventory(&inventory, cx),
            [StateEvent::InventoryChanged]
        );
        let reread = take_read(&mut receiver);
        cx.read(|cx| {
            let state = AppState::try_read(cx).expect("state");
            assert!(matches!(state.disable_keys_load_for(&key), Load::Loading));
            assert!(state.disable_keys_reads.confirmed(&key).is_none());
            assert!(!state.disable_keys_controls_enabled(&key));
        });

        cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::CAPS_LOCK));
        assert!(
            receiver.try_recv().is_err(),
            "the public write path must wait for the revision re-read"
        );

        reread.reply.send(Ok(original)).expect("revision re-read");
        cx.run_until_parked();
    }

    #[gpui::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep repeated external edits, stale receipts and read ordering in one lifecycle regression"
    )]
    fn file_changed_after_save_reconciles_disable_keys_without_resaving(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().expect("temporary config directory");
        let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
        Config::default()
            .save_to_path(&path)
            .expect("seed config file");
        let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) =
            test_state(ConfigPersistence::UserFile(file), &[inventory]);
        install_reads(state, cx);
        let before = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver)
            .reply
            .send(Ok(before))
            .expect("initial read");
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));
        let written = DisableKeysState {
            disabled: DisableKeysMask::CAPS_LOCK,
            ..before
        };
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                assert_eq!(
                    state.apply_disable_keys_write_result(context.clone(), Ok(written)),
                    [StateEvent::DisableKeysChanged(key.clone())]
                );
            });
        });
        let reload_context = take_disable_keys_reload(&mut receiver);

        let mut external = Config::load_from_path(&path).expect("external config");
        external.keyboard.bindings.insert(
            "f1".parse().expect("F-key trigger"),
            openlogi_core::binding::Action::Paste,
        );
        external.set_disabled_keys(key.as_str(), BTreeSet::from([DisableKey::WindowsCommand]));
        external
            .save_to_path(&path)
            .expect("external revision B seen by the agent");
        external.set_disabled_keys(key.as_str(), BTreeSet::new());
        external
            .save_to_path(&path)
            .expect("external revision C before GUI refresh");

        let events = cx.update(|cx| {
            AppState::update(cx, |state, _| {
                state.apply_disable_keys_reload_result(
                    reload_context,
                    Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave),
                )
            })
        });
        assert_eq!(
            events,
            [
                StateEvent::DisableKeysChanged(key.clone()),
                StateEvent::SettingsChanged,
                StateEvent::BindingsChanged(key.clone()),
                StateEvent::DpiChanged(key.clone()),
            ]
        );
        cx.read(|cx| {
            let state = AppState::try_read(cx).expect("state");
            assert_eq!(state.keyboard_bindings(), &external.keyboard.bindings);
            assert_eq!(
                state.config.disabled_keys(key.as_str()),
                Some(&BTreeSet::new())
            );
            assert!(
                matches!(
                    state.disable_keys_status(&key),
                    Some(DisableKeysPersistenceStatus::AwaitingReconciliationReload(
                        _
                    ))
                ),
                "a valid GUI refresh must not claim the agent already loaded that revision"
            );
        });
        cx.run_until_parked();
        let (mut reload_c, initial_stale_read) = take_reconciliation_commands(&mut receiver);
        assert_eq!(
            reload_c.revision,
            openlogi_core::config::ConfigRevision::of(&std::fs::read_to_string(&path).unwrap())
        );
        let ConfigReloadContext::DisableKeys(context_c) = reload_c.contexts.remove(0).context()
        else {
            panic!("contextual reload")
        };

        let externally_confirmed = DisableKeysState {
            disabled: DisableKeysMask::WINDOWS_COMMAND,
            ..before
        };
        initial_stale_read
            .reply
            .send(Ok(externally_confirmed))
            .expect("agent still reports revision B hardware");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                !AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });

        // Another edit overtakes the confirmation of C. Refreshing D must
        // likewise retain the gate until its own receipt and subsequent read.
        external.set_disabled_keys(key.as_str(), BTreeSet::from([DisableKey::WindowsCommand]));
        external.save_to_path(&path).expect("external revision D");
        let final_text = std::fs::read_to_string(&path).unwrap();
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_reload_result(
                    context_c.clone(),
                    Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave),
                )
            });
        });
        cx.run_until_parked();
        let (mut reload_d, superseded_read) = take_reconciliation_commands(&mut receiver);
        assert_eq!(
            reload_d.revision,
            openlogi_core::config::ConfigRevision::of(&final_text)
        );
        let ConfigReloadContext::DisableKeys(context_d) = reload_d.contexts.remove(0).context()
        else {
            panic!("contextual reload")
        };
        assert_ne!(context_c.request_id, context_d.request_id);
        superseded_read.reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            AppState::update(cx, |state, _| {
                assert!(
                    state
                        .apply_disable_keys_reload_result(
                            context_c,
                            Ok(openlogi_ipc::SavedConfigVerdict::Accepted)
                        )
                        .is_empty(),
                    "old receipt must not acknowledge D"
                );
                assert!(!state.disable_keys_controls_enabled(&key));
            });
        });
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_reload_result(
                    context_d,
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
                )
            });
        });
        cx.run_until_parked();
        let post_receipt_read = take_read(&mut receiver);
        cx.read(|cx| {
            assert!(
                !AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });
        post_receipt_read
            .reply
            .send(Ok(externally_confirmed))
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            final_text,
            "reconciliation never saves a superseded policy"
        );
        assert_eq!(
            Config::load_from_path(&path)
                .expect("external config remains on disk")
                .disabled_keys(key.as_str()),
            Some(&BTreeSet::from([DisableKey::WindowsCommand])),
            "the stale Caps Lock choice must not be saved again"
        );
        assert!(
            receiver.try_recv().is_err(),
            "only contextual reloads and reads were required, not manual writes"
        );
    }

    #[gpui::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the malformed-file, retry, receipt and post-receipt read lifecycle in one regression"
    )]
    fn failed_external_reconciliation_keeps_controls_blocked_after_a_fresh_read(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().expect("temporary config directory");
        let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
        Config::default().save_to_path(&path).expect("seed config");
        let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (state, mut receiver, key) =
            test_state(ConfigPersistence::UserFile(file), &[inventory]);
        install_reads(state, cx);
        let before = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver).reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        let context = cx.update(|cx| AppState::update(cx, |state, _| begin(state, &mut receiver)));
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_write_result(
                    context,
                    Ok(DisableKeysState {
                        disabled: DisableKeysMask::CAPS_LOCK,
                        ..before
                    }),
                )
            });
        });
        let reload = take_disable_keys_reload(&mut receiver);
        // The agent accepted a valid external revision, but another editor left
        // malformed text before the GUI could refresh that file.
        std::fs::write(&path, "[malformed").unwrap();
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_reload_result(
                    reload,
                    Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave),
                )
            });
        });
        cx.run_until_parked();
        take_read(&mut receiver).reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(matches!(state.disable_keys_load_for(&key), Load::Ready(_)));
            assert!(
                !state.disable_keys_controls_enabled(&key),
                "device freshness cannot substitute for failed config reconciliation"
            );
            assert_eq!(
                state.disable_keys_error(&key),
                Some(DisableKeysError::RefreshFailed)
            );
        });
        cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::WINDOWS_COMMAND));
        assert!(
            receiver.try_recv().is_err(),
            "no write from stale local configuration"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[malformed");

        // A retry cannot turn the same parse error into an acknowledgement.
        cx.update(|cx| AppState::retry_disable_keys_reload(cx, &key));
        cx.run_until_parked();
        take_read(&mut receiver).reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.disable_keys_controls_enabled(&key));
            assert_eq!(
                state.disable_keys_error(&key),
                Some(DisableKeysError::RefreshFailed)
            );
        });

        let mut external = Config::default();
        external.set_disabled_keys(key.as_str(), BTreeSet::from([DisableKey::WindowsCommand]));
        external
            .keyboard
            .bindings
            .insert("f1".parse().unwrap(), openlogi_core::binding::Action::Paste);
        let repaired_path = directory.path().join("repaired.toml");
        external.save_to_path(&repaired_path).unwrap();
        std::fs::rename(repaired_path, &path).unwrap();
        let repaired_text = std::fs::read_to_string(&path).unwrap();
        cx.update(|cx| AppState::retry_disable_keys_reload(cx, &key));
        cx.run_until_parked();
        let mut receipt = None;
        let mut early_read = None;
        while let Ok(command) = receiver.try_recv() {
            match command {
                Command::ReloadConfig(mut reload) => {
                    assert!(receipt.is_none(), "one reload for the repaired file");
                    assert_eq!(
                        reload.revision,
                        openlogi_core::config::ConfigRevision::of(&repaired_text)
                    );
                    let ConfigReloadContext::DisableKeys(context) =
                        reload.contexts.remove(0).context()
                    else {
                        panic!("contextual reload");
                    };
                    receipt = Some(context);
                }
                Command::ReadDisableKeys(read) => {
                    assert!(early_read.replace(read).is_none(), "one refresh read");
                }
                _ => panic!("reconciliation must not submit manual hardware writes"),
            }
        }
        // A read racing ahead of agent reload cannot make the UI writable.
        early_read.unwrap().reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.disable_keys_controls_enabled(&key));
            assert_eq!(state.keyboard_bindings(), &external.keyboard.bindings);
        });
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_reload_result(
                    receipt.unwrap(),
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
                )
            });
        });
        cx.run_until_parked();
        let final_read = take_read(&mut receiver);
        cx.read(|cx| {
            assert!(
                !AppState::try_read(cx)
                    .unwrap()
                    .disable_keys_controls_enabled(&key)
            );
        });
        final_read
            .reply
            .send(Ok(DisableKeysState {
                disabled: DisableKeysMask::WINDOWS_COMMAND,
                ..before
            }))
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.disable_keys_controls_enabled(&key));
            assert_eq!(state.disable_keys_error(&key), None);
            assert_eq!(
                state.config.disabled_keys(key.as_str()),
                Some(&BTreeSet::from([DisableKey::WindowsCommand]))
            );
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            repaired_text,
            "recovery never overwrites external configuration"
        );
        assert!(
            receiver.try_recv().is_err(),
            "only reload and read commands are needed"
        );
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
        let trigger = "f1".parse().expect("F-key trigger");
        let mut external_config = Config::load_from_path(&path).expect("external config");
        external_config.keyboard.bindings.insert(
            trigger,
            openlogi_core::binding::Action::TypeText("external binding".into()),
        );
        external_config.save_to_path(&path).expect("external save");
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

        assert_eq!(
            state.retry_disable_keys_save_inner(&key),
            [
                StateEvent::DisableKeysChanged(key.clone()),
                StateEvent::SettingsChanged,
                StateEvent::BindingsChanged(key.clone()),
                StateEvent::DpiChanged(key.clone()),
            ]
        );
        assert_eq!(
            state.keyboard_bindings(),
            &external_config.keyboard.bindings
        );
        assert_eq!(
            state.keyboard_bindings(),
            &Config::load_from_path(&path)
                .expect("saved config")
                .keyboard
                .bindings
        );
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

    struct SaveRetryFixture {
        directory: tempfile::TempDir,
        state: AppState,
        receiver: tokio::sync::mpsc::UnboundedReceiver<Command>,
        key: DeviceKey,
        external: Config,
    }

    impl SaveRetryFixture {
        fn new(inventories: &[DeviceInventory]) -> Self {
            let directory = tempfile::tempdir().expect("temp config");
            let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
            Config::default().save_to_path(&path).expect("seed config");
            let (_, file) = ConfigFile::load_from_path(&path).expect("tracked config");
            let (mut state, mut receiver, key) =
                test_state(ConfigPersistence::UserFile(file), inventories);
            let context = begin(&mut state, &mut receiver);
            // Exercise recovery for a device other than the one now on screen.
            let _ = state.select_device(inventories.len() - 1);
            while receiver.try_recv().is_ok() {}
            let mut external = Config::load_from_path(&path).expect("external config");
            external.keyboard.bindings.insert(
                "f1".parse().expect("F-key trigger"),
                openlogi_core::binding::Action::Paste,
            );
            external.app_settings.smooth_scroll = true;
            let current = state
                .current_record()
                .expect("selected device")
                .device_key();
            external.set_dpi(current.as_str(), super::super::Dpi::new(2400));
            external.save_to_path(&path).expect("external save");
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
            assert!(receiver.try_recv().is_err());
            Self {
                directory,
                state,
                receiver,
                key,
                external,
            }
        }

        fn path(&self) -> std::path::PathBuf {
            self.directory
                .path()
                .join(openlogi_core::paths::CONFIG_FILE)
        }
    }

    #[test]
    fn save_retry_failed_refresh_keeps_projections_and_recovery() {
        let mut fixture = SaveRetryFixture::new(&[keyboard_inventory(0xb35b, [1, 2, 3, 4])]);
        let previous = fixture.state.keyboard_bindings().clone();
        std::fs::write(fixture.path(), "[invalid TOML").expect("external invalid edit");
        assert_eq!(
            fixture.state.retry_disable_keys_save_inner(&fixture.key),
            [
                StateEvent::DisableKeysChanged(fixture.key.clone()),
                StateEvent::SettingsChanged
            ]
        );
        assert_eq!(fixture.state.keyboard_bindings(), &previous);
        assert!(!fixture.state.app_settings().smooth_scroll);
        assert!(fixture.state.disable_keys_error(&fixture.key).is_some());
        assert!(matches!(
            fixture.state.disable_keys_status(&fixture.key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        assert!(fixture.receiver.try_recv().is_err());
    }

    #[test]
    fn save_retry_locked_writer_keeps_refreshed_projections_and_can_retry() {
        let mut fixture = SaveRetryFixture::new(&[keyboard_inventory(0xb35b, [1, 2, 3, 4])]);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.path().with_added_extension("lock"))
            .expect("existing config lock");
        lock.try_lock().expect("hold competing writer lock");
        let events = fixture.state.retry_disable_keys_save_inner(&fixture.key);
        assert_eq!(
            events,
            [
                StateEvent::DisableKeysChanged(fixture.key.clone()),
                StateEvent::SettingsChanged,
                StateEvent::BindingsChanged(fixture.key.clone()),
                StateEvent::DpiChanged(fixture.key.clone()),
            ]
        );
        assert_eq!(
            fixture.state.keyboard_bindings(),
            &fixture.external.keyboard.bindings
        );
        assert!(fixture.state.app_settings().smooth_scroll);
        assert_eq!(fixture.state.dpi(), super::super::Dpi::new(2400));
        assert_eq!(
            fixture.state.config.disabled_keys(fixture.key.as_str()),
            None
        );
        assert!(fixture.state.disable_keys_error(&fixture.key).is_some());
        assert!(matches!(
            fixture.state.disable_keys_status(&fixture.key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        assert!(fixture.receiver.try_recv().is_err());
        drop(lock);
        assert_eq!(
            fixture.state.retry_disable_keys_save_inner(&fixture.key),
            events
        );
        take_disable_keys_reload(&mut fixture.receiver);
        assert!(fixture.receiver.try_recv().is_err());
        assert_eq!(
            Config::load_from_path(&fixture.path())
                .expect("saved config")
                .disabled_keys(fixture.key.as_str()),
            Some(&BTreeSet::from([DisableKey::CapsLock]))
        );
    }

    #[gpui::test]
    fn save_retry_fences_other_keyboards_until_receipt_and_post_receipt_read(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = SaveRetryFixture::new(&[
            keyboard_inventory(0xb35b, [1, 2, 3, 4]),
            keyboard_inventory(0xb35c, [5, 6, 7, 8]),
        ]);
        let other = fixture.state.current_record().unwrap().device_key();
        let offline = DeviceKey::from("offline-keyboard");
        fixture
            .state
            .disable_keys_reads
            .set_confirmed(&offline, DisableKeysState::default());
        fixture
            .external
            .set_disabled_keys(other.as_str(), BTreeSet::from([DisableKey::CapsLock]));
        fixture.external.save_to_path(&fixture.path()).unwrap();
        let SaveRetryFixture {
            directory: _directory,
            state,
            mut receiver,
            key,
            ..
        } = fixture;
        install_reads(state, cx);
        let before = DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::NUM_LOCK,
            disabled: DisableKeysMask::EMPTY,
        };
        take_read(&mut receiver).reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.disable_keys_controls_enabled(&other));
        });
        cx.update(|cx| AppState::retry_disable_keys_save(cx, &key));
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(matches!(state.disable_keys_load_for(&other), Load::Loading));
            assert!(state.disable_keys_reads.confirmed(&other).is_none());
            assert!(state.disable_keys_reads.confirmed(&offline).is_none());
            assert!(!state.disable_keys_controls_enabled(&other));
            assert!(matches!(
                state.disable_keys_status(&key),
                Some(DisableKeysPersistenceStatus::AwaitingReload(_))
            ));
        });
        cx.run_until_parked();
        let (reload, early_read) = take_reconciliation_commands(&mut receiver);
        let ConfigReloadContext::DisableKeys(context) = reload.contexts[0].context() else {
            panic!("Save Retry keeps its Saved reload purpose");
        };
        assert_eq!(context.key, key);
        early_read.reply.send(Ok(before)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::NUM_LOCK));
        assert!(
            receiver.try_recv().is_err(),
            "no write from a pre-receipt snapshot"
        );
        cx.update(|cx| {
            AppState::apply(cx, |state| {
                state.apply_disable_keys_reload_result(
                    context,
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
                )
            });
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.disable_keys_controls_enabled(&other));
        });
        take_read(&mut receiver)
            .reply
            .send(Ok(DisableKeysState {
                disabled: DisableKeysMask::CAPS_LOCK,
                ..before
            }))
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.disable_keys_controls_enabled(&other));
            assert_eq!(
                state.config.disabled_keys(other.as_str()),
                Some(&BTreeSet::from([DisableKey::CapsLock]))
            );
            let Load::Ready(snapshot) = state.disable_keys_load_for(&other) else {
                panic!("post-receipt snapshot");
            };
            assert_eq!(snapshot.disabled, DisableKeysMask::CAPS_LOCK);
        });
        assert!(
            receiver.try_recv().is_err(),
            "Save Retry never resends A's hardware write"
        );
    }

    struct RestoreLocale(String);

    impl Drop for RestoreLocale {
        fn drop(&mut self) {
            rust_i18n::set_locale(&self.0);
        }
    }

    fn assert_russian_locale() {
        let locale = rust_i18n::locale().to_string();
        assert_eq!(locale, "ru");
    }

    #[gpui::test]
    fn save_retry_activates_external_language_and_emits_language_changed(
        cx: &mut gpui::TestAppContext,
    ) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let _restore = RestoreLocale(rust_i18n::locale().to_string());
        rust_i18n::set_locale("en");
        let mut fixture = SaveRetryFixture::new(&[keyboard_inventory(0xb35b, [1, 2, 3, 4])]);
        fixture.external.app_settings.language = Some("ru".into());
        fixture.external.save_to_path(&fixture.path()).unwrap();
        let SaveRetryFixture {
            directory: _directory,
            state,
            mut receiver,
            key,
            ..
        } = fixture;
        let observed = cx.update(|cx| {
            let state = cx.new(|_| state);
            AppState::set_global(state.clone(), cx);
            let observed = cx.new(|cx: &mut gpui::Context<Vec<StateEvent>>| {
                cx.subscribe(&state, |events, source, event: &StateEvent, cx| {
                    if event == &StateEvent::LanguageChanged {
                        assert_eq!(source.read(cx).language(), Some("ru"));
                        assert_russian_locale();
                    }
                    events.push(event.clone());
                })
                .detach();
                Vec::new()
            });
            AppState::retry_disable_keys_save(cx, &key);
            observed
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(observed.read(cx).contains(&StateEvent::LanguageChanged));
            assert_eq!(AppState::try_read(cx).unwrap().language(), Some("ru"));
            assert_russian_locale();
        });
        take_disable_keys_reload(&mut receiver);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn save_retry_activates_external_language_even_when_the_save_fails_again() {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let _restore = RestoreLocale(rust_i18n::locale().to_string());
        rust_i18n::set_locale("en");
        let mut fixture = SaveRetryFixture::new(&[keyboard_inventory(0xb35b, [1, 2, 3, 4])]);
        fixture.external.app_settings.language = Some("ru".into());
        fixture.external.save_to_path(&fixture.path()).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.path().with_added_extension("lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let events = fixture.state.retry_disable_keys_save_inner(&fixture.key);
        assert!(events.contains(&StateEvent::LanguageChanged));
        assert_eq!(fixture.state.language(), Some("ru"));
        assert_russian_locale();
        assert!(matches!(
            fixture.state.disable_keys_status(&fixture.key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
        assert_eq!(
            fixture.state.disable_keys_error(&fixture.key),
            Some(DisableKeysError::SaveFailed)
        );
        assert!(!fixture.state.disable_keys_controls_enabled(&fixture.key));
        assert!(fixture.receiver.try_recv().is_err());
    }

    #[test]
    fn save_retry_without_recovery_is_a_noop() {
        let (mut state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            &[keyboard_inventory(0xb35b, [1, 2, 3, 4])],
        );
        assert!(state.retry_disable_keys_save_inner(&key).is_empty());
        assert!(receiver.try_recv().is_err());
    }

    #[gpui::test]
    fn save_retry_action_delivers_projection_events_for_current_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = SaveRetryFixture::new(&[
            keyboard_inventory(0xb35b, [1, 2, 3, 4]),
            keyboard_inventory(0xb35c, [5, 6, 7, 8]),
        ]);
        let SaveRetryFixture {
            directory: _directory,
            state,
            mut receiver,
            key,
            external,
        } = fixture;
        let current = state
            .current_record()
            .expect("selected device")
            .device_key();
        assert_ne!(current, key);
        let observer = cx.update(|cx| {
            let state = cx.new(|_| state);
            AppState::set_global(state.clone(), cx);
            let observer = cx.new(|cx: &mut gpui::Context<Vec<StateEvent>>| {
                cx.subscribe(&state, |events, source, event: &StateEvent, cx| {
                    // Subscribers must see the refreshed values at delivery time.
                    assert_eq!(
                        source.read(cx).keyboard_bindings(),
                        &source.read(cx).config.keyboard.bindings
                    );
                    events.push(event.clone());
                })
                .detach();
                Vec::new()
            });
            AppState::retry_disable_keys_save(cx, &key);
            observer
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                observer.read(cx),
                &vec![
                    StateEvent::DisableKeysChanged(key.clone()),
                    StateEvent::DisableKeysChanged(current.clone()),
                    StateEvent::SettingsChanged,
                    StateEvent::BindingsChanged(current.clone()),
                    StateEvent::DpiChanged(current.clone()),
                ]
            );
            let state = AppState::try_read(cx).expect("state");
            assert_eq!(state.keyboard_bindings(), &external.keyboard.bindings);
            assert_eq!(
                state
                    .current_record()
                    .expect("selected device")
                    .device_key(),
                current
            );
            assert_eq!(state.dpi(), super::super::Dpi::new(2400));
        });
        assert_eq!(take_disable_keys_reload(&mut receiver).key, key);
        assert!(receiver.try_recv().is_err());
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
            Some(DisableKeysPersistenceStatus::SavedNotReloaded { .. })
        ));

        assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
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
    fn reconciliation_reload_survives_disconnect_and_closed_ipc() {
        let inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) =
            test_state(ConfigPersistence::MemoryOnly, &[inventory]);
        let context = state.allocate_disable_keys_context(&key).unwrap();
        state.request_disable_keys_reload(context.clone(), ReloadPurpose::Reconciled);
        assert_eq!(take_disable_keys_reload(&mut receiver), context);
        state.invalidate_disable_keys(&key);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::ReconcileRequired { .. })
        ));
        assert!(
            state
                .apply_disable_keys_reload_result(
                    context.clone(),
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted)
                )
                .is_empty(),
            "late acknowledgement cannot retire detached reconciliation"
        );

        state
            .disable_keys_reads
            .install_generation_for_test(key.clone(), context.route_generation + 1);
        assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
        let current = take_disable_keys_reload(&mut receiver);
        assert_ne!(current.route_generation, context.route_generation);
        let _ = state.apply_disable_keys_reload_result(
            current,
            Err(openlogi_ipc::ConfigReloadError {
                message: "external file changed again".into(),
            }),
        );
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::ReconcileRequired { .. })
        ));
        assert_eq!(
            state.disable_keys_error(&key),
            Some(DisableKeysError::ReloadFailed)
        );

        drop(receiver);
        assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::ReconcileRequired { .. })
        ));
        assert_eq!(
            state.disable_keys_error(&key),
            Some(DisableKeysError::AgentUnavailable)
        );
        assert!(!state.disable_keys_controls_enabled(&key));
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

        assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded { .. })
        ));
        assert_eq!(
            state.disable_keys_error(&key),
            Some(DisableKeysError::AgentUnavailable)
        );
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

        assert_eq!(
            state.retry_disable_keys_save_inner(&key),
            [
                StateEvent::DisableKeysChanged(key.clone()),
                StateEvent::SettingsChanged,
                StateEvent::BindingsChanged(key.clone()),
                StateEvent::DpiChanged(key.clone()),
            ]
        );
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloaded { .. })
        ));
        assert_eq!(
            state.disable_keys_error(&key),
            Some(DisableKeysError::AgentUnavailable)
        );
        assert_eq!(
            ConfigFile::load_from_path(&path)
                .expect("saved config")
                .0
                .disabled_keys(key.as_str()),
            Some(&BTreeSet::from([DisableKey::CapsLock]))
        );
    }

    fn take_config_reconciliation(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> (u64, openlogi_core::config::ConfigRevision) {
        let Ok(Command::ReloadConfig(reload)) = receiver.try_recv() else {
            panic!("expected config confirmation, never a hardware write");
        };
        let request_id = reload
            .contexts
            .iter()
            .find_map(|context| match context.context() {
                ConfigReloadContext::Reconciled(id) => Some(id),
                _ => None,
            })
            .expect("revision-specific receipt");
        assert!(receiver.try_recv().is_err());
        (request_id, reload.revision)
    }

    #[test]
    fn general_reconciliation_repeats_for_new_revisions_and_rejects_old_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
        let (_, file) = ConfigFile::load_from_path(&path).unwrap();
        let (mut state, mut receiver, key) = test_state(
            ConfigPersistence::UserFile(file),
            &[keyboard_inventory(0xb35b, [1, 2, 3, 4])],
        );
        let mismatch = Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave);
        let _ = state.apply_saved_config_reload_result(mismatch.clone());
        let (first, _) = take_config_reconciliation(&mut receiver);
        let mut external = Config::load_from_path(&path).unwrap();
        external.set_disabled_keys(key.as_str(), BTreeSet::from([DisableKey::CapsLock]));
        external.save_to_path(&path).unwrap();
        let _ = state.apply_reconciled_config_reload_result(first, mismatch);
        let (second, revision) = take_config_reconciliation(&mut receiver);
        assert_ne!(first, second);
        assert_eq!(
            revision,
            openlogi_core::config::ConfigRevision::of(&std::fs::read_to_string(&path).unwrap())
        );
        state
            .disable_keys_reads
            .set_confirmed(&key, DisableKeysState::default());
        assert!(
            state
                .apply_reconciled_config_reload_result(
                    first,
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted)
                )
                .is_empty()
        );
        assert!(!state.disable_keys_controls_enabled(&key));
        let _ = state.apply_reconciled_config_reload_result(
            second,
            Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
        );
        assert!(matches!(state.disable_keys_load_for(&key), Load::Unknown));
        assert!(
            !state.disable_keys_controls_enabled(&key),
            "the pre-receipt snapshot was retired"
        );
    }

    #[test]
    fn general_refresh_and_reload_failures_keep_a_working_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
        let (_, file) = ConfigFile::load_from_path(&path).unwrap();
        let (mut state, mut receiver, key) = test_state(
            ConfigPersistence::UserFile(file),
            &[keyboard_inventory(0xb35b, [1, 2, 3, 4])],
        );
        let valid = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, "[malformed").unwrap();
        let _ = state.apply_saved_config_reload_result(Ok(
            openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave,
        ));
        for _ in 0..2 {
            state
                .disable_keys_reads
                .set_confirmed(&key, DisableKeysState::default());
            assert!(!state.disable_keys_controls_enabled(&key));
            assert_eq!(
                state.disable_keys_config_reconciliation(),
                &ConfigReconciliation::Required(DisableKeysError::RefreshFailed)
            );
            assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
            assert!(receiver.try_recv().is_err());
        }
        std::fs::write(&path, &valid).unwrap();
        let _ = state.retry_disable_keys_reload_inner(&key);
        let (first, revision) = take_config_reconciliation(&mut receiver);
        assert_eq!(revision, openlogi_core::config::ConfigRevision::of(&valid));
        let _ = state.apply_reconciled_config_reload_result(
            first,
            Err(openlogi_ipc::ConfigReloadError {
                message: "retryable".into(),
            }),
        );
        assert_eq!(
            state.disable_keys_config_reconciliation(),
            &ConfigReconciliation::Required(DisableKeysError::ReloadFailed)
        );
        let _ = state.retry_disable_keys_reload_inner(&key);
        let (second, _) = take_config_reconciliation(&mut receiver);
        assert_ne!(first, second);
        assert!(
            state
                .apply_reconciled_config_reload_result(
                    first,
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted)
                )
                .is_empty()
        );
        assert!(!state.disable_keys_controls_enabled(&key));
        let _ = state.apply_reconciled_config_reload_result(
            second,
            Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
        );
        assert_eq!(
            state.disable_keys_config_reconciliation(),
            &ConfigReconciliation::Confirmed
        );
        assert!(
            !state.disable_keys_controls_enabled(&key),
            "retry still needs a post-receipt read"
        );
    }

    #[test]
    fn later_general_save_supersedes_pending_confirmation_and_closed_ipc_keeps_retry() {
        let (mut state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            &[keyboard_inventory(0xb35b, [1, 2, 3, 4])],
        );
        let _ = state.apply_saved_config_reload_result(Ok(
            openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave,
        ));
        let (first, _) = take_config_reconciliation(&mut receiver);
        let _ = state.commit_smooth_scroll(true);
        let (second, _) = take_config_reconciliation(&mut receiver);
        assert_ne!(first, second);
        assert!(
            state
                .apply_reconciled_config_reload_result(
                    first,
                    Ok(openlogi_ipc::SavedConfigVerdict::Accepted)
                )
                .is_empty()
        );
        let _ =
            state.apply_saved_config_reload_result(Ok(openlogi_ipc::SavedConfigVerdict::Accepted));
        assert!(!state.disable_keys_controls_enabled(&key));
        drop(receiver);
        let _ = state.apply_reconciled_config_reload_result(
            second,
            Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave),
        );
        assert_eq!(
            state.disable_keys_config_reconciliation(),
            &ConfigReconciliation::Required(DisableKeysError::AgentUnavailable)
        );
        assert!(!state.retry_disable_keys_reload_inner(&key).is_empty());
        assert!(!state.disable_keys_controls_enabled(&key));
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
            Some(DisableKeysPersistenceStatus::Idle { .. })
        ));

        state
            .disable_keys_reads
            .install_generation_for_test(key.clone(), context.route_generation + 1);
        state
            .disable_keys_phase(&key)
            .reload_sent(context.clone(), ReloadPurpose::Saved);
        state.invalidate_disable_keys(&key);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::SavedNotReloadedDetached { .. })
        ));
    }

    /// One value per fact the write gate reads, every one of them open.
    struct GateFacts {
        reconciliation: ConfigReconciliation,
        config_writable: bool,
        online: bool,
        persistent: bool,
        load: DisableKeysLoad,
        phase: Option<DisableKeysPersistenceStatus>,
    }

    impl GateFacts {
        fn open() -> Self {
            Self {
                reconciliation: ConfigReconciliation::Confirmed,
                config_writable: true,
                online: true,
                persistent: true,
                load: Load::Ready(std::sync::Arc::new(DisableKeysState::default())),
                phase: None,
            }
        }

        fn is_open(&self) -> bool {
            WriteGate {
                reconciliation: &self.reconciliation,
                config_writable: self.config_writable,
                online: self.online,
                persistent: self.persistent,
                load: &self.load,
                phase: self.phase.as_ref(),
            }
            .is_open()
        }
    }

    fn gate_context() -> DisableKeysRequestContext {
        DisableKeysRequestContext {
            key: DeviceKey::from("keyboard"),
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb35b,
            },
            route_generation: 7,
            request_id: 9,
        }
    }

    #[test]
    fn write_gate_opens_for_an_untouched_or_idle_device() {
        assert!(GateFacts::open().is_open());
        for phase in [
            DisableKeysPersistenceStatus::default(),
            DisableKeysPersistenceStatus::Idle {
                error: Some(DisableKeysError::WriteFailed),
            },
        ] {
            let facts = GateFacts {
                phase: Some(phase),
                ..GateFacts::open()
            };
            assert!(facts.is_open(), "{:?}", facts.phase);
        }
    }

    #[test]
    fn write_gate_needs_a_confirmed_config_reconciliation() {
        for reconciliation in [
            ConfigReconciliation::Required(DisableKeysError::ReloadFailed),
            ConfigReconciliation::Awaiting(ConfigReloadContext::Reconciled(1)),
            ConfigReconciliation::Awaiting(ConfigReloadContext::DisableKeys(gate_context())),
        ] {
            let facts = GateFacts {
                reconciliation,
                ..GateFacts::open()
            };
            assert!(!facts.is_open(), "{:?}", facts.reconciliation);
        }
    }

    #[test]
    fn write_gate_needs_a_writable_config() {
        let facts = GateFacts {
            config_writable: false,
            ..GateFacts::open()
        };
        assert!(!facts.is_open());
    }

    #[test]
    fn write_gate_needs_an_online_record() {
        let facts = GateFacts {
            online: false,
            ..GateFacts::open()
        };
        assert!(!facts.is_open());
    }

    #[test]
    fn write_gate_needs_a_persistent_record() {
        let facts = GateFacts {
            persistent: false,
            ..GateFacts::open()
        };
        assert!(!facts.is_open());
    }

    #[test]
    fn write_gate_needs_a_ready_snapshot() {
        for load in [
            Load::Unknown,
            Load::Loading,
            Load::Failed("read failed".into()),
            Load::Unsupported("unsupported".into()),
        ] {
            let facts = GateFacts {
                load,
                ..GateFacts::open()
            };
            assert!(!facts.is_open(), "{:?}", facts.load);
        }
    }

    #[test]
    fn write_gate_needs_an_idle_transaction() {
        let context = gate_context();
        let recovery = DisableKeysRecoveryToken::from(context.clone());
        for phase in [
            DisableKeysPersistenceStatus::Applying(context.clone()),
            DisableKeysPersistenceStatus::AppliedNotSaved {
                recovery: recovery.clone(),
                confirmed: DisableKeysState::default(),
                error: DisableKeysError::SaveFailed,
            },
            DisableKeysPersistenceStatus::AwaitingReload(context.clone()),
            DisableKeysPersistenceStatus::SavedNotReloaded {
                context: context.clone(),
                error: DisableKeysError::ReloadFailed,
            },
            DisableKeysPersistenceStatus::SavedNotReloadedDetached {
                recovery: recovery.clone(),
                error: None,
            },
            DisableKeysPersistenceStatus::ReconcileRequired {
                recovery,
                error: None,
            },
            DisableKeysPersistenceStatus::AwaitingReconciliationReload(context),
        ] {
            let facts = GateFacts {
                phase: Some(phase),
                ..GateFacts::open()
            };
            assert!(!facts.is_open(), "{:?}", facts.phase);
        }
    }

    /// An agent restart re-enumerates the keyboard, but the IPC client still
    /// holds the reload and delivers it to the successor. The pending
    /// confirmation therefore waits for that answer instead of failing.
    #[test]
    fn agent_restart_leaves_a_pending_reconciliation_to_the_successors_receipt() {
        let mut inventory = keyboard_inventory(0xb35b, [1, 2, 3, 4]);
        let (mut state, mut receiver, key) = test_state(
            ConfigPersistence::MemoryOnly,
            std::slice::from_ref(&inventory),
        );
        let _ = state.apply_saved_config_reload_result(Ok(
            openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave,
        ));
        let (request_id, _) = take_config_reconciliation(&mut receiver);

        for online in [false, true] {
            inventory.paired[0].online = online;
            let _ = state.refresh_inventories(
                std::slice::from_ref(&inventory),
                &[],
                &AssetResolver::new(),
                &[],
            );
            assert_eq!(
                state.disable_keys_config_reconciliation(),
                &ConfigReconciliation::Awaiting(ConfigReloadContext::Reconciled(request_id))
            );
            assert!(!state.disable_keys_controls_enabled(&key));
        }

        let _ = state.apply_reconciled_config_reload_result(
            request_id,
            Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
        );
        assert_eq!(
            state.disable_keys_config_reconciliation(),
            &ConfigReconciliation::Confirmed
        );
    }
}

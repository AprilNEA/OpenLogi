//! Exact-revision confirmation after adopting an externally changed file.

use crate::services::ipc::{ConfigReloadContext, ReloadConfig};

use super::{AppState, ConfigReconciliation, DisableKeysError, StateEvent, StateEvents};

impl AppState {
    /// Adopt the external file and request acknowledgement of that exact revision.
    pub(super) fn reconcile_saved_config(&mut self) -> StateEvents {
        let events = match self.reconcile_external_config() {
            Ok(events) => events,
            Err(events) => return events,
        };
        let request_id = self.disable_keys_reads.next_config_reload();
        self.disable_keys_reads
            .track_config_reload(ConfigReloadContext::Reconciled(request_id));
        self.send_config_reload(ReloadConfig::reconciled(request_id, self.config.revision()));
        events
    }

    /// A later local save must not let an earlier receipt release the fence.
    pub(super) fn track_external_config_reload(&mut self, reload: &mut ReloadConfig) {
        if self.disable_keys_reads.reconciliation() == &ConfigReconciliation::Confirmed {
            return;
        }
        let request_id = self.disable_keys_reads.next_config_reload();
        reload.track_reconciliation(request_id);
        self.disable_keys_reads
            .track_config_reload(ConfigReloadContext::Reconciled(request_id));
    }

    /// A stopped IPC worker leaves the shared Reload Retry recovery available.
    pub(super) fn send_config_reload(&mut self, reload: ReloadConfig) {
        if !self.send_ipc(reload)
            && let ConfigReconciliation::Awaiting(context) =
                self.disable_keys_reads.reconciliation()
        {
            let context = context.clone();
            self.disable_keys_reads
                .fail_config_reload(&context, DisableKeysError::AgentUnavailable);
        }
    }

    /// Deliver only the latest route-independent revision acknowledgement.
    pub(crate) fn apply_reconciled_config_reload_result(
        &mut self,
        request_id: u64,
        result: Result<openlogi_ipc::SavedConfigVerdict, openlogi_ipc::ConfigReloadError>,
    ) -> StateEvents {
        let context = ConfigReloadContext::Reconciled(request_id);
        if !self.disable_keys_reads.config_reload_matches(&context) {
            return StateEvents::none();
        }
        match result {
            Ok(openlogi_ipc::SavedConfigVerdict::Accepted) => {
                self.confirm_external_config_reload(&context)
            }
            Ok(openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave) => {
                self.reconcile_saved_config()
            }
            Err(error) => {
                tracing::warn!(error = %error.message, "could not confirm externally edited configuration");
                self.disable_keys_reads
                    .fail_config_reload(&context, DisableKeysError::ReloadFailed);
                self.disable_keys_config_events()
            }
        }
    }

    /// Retire snapshots collected before the matching agent receipt.
    pub(super) fn confirm_external_config_reload(
        &mut self,
        context: &ConfigReloadContext,
    ) -> StateEvents {
        let mut events = StateEvents::none();
        for key in self.disable_keys_reads.confirm_config_reload(context) {
            events = events.and(StateEvent::DisableKeysChanged(key));
        }
        events.and(self.disable_keys_config_events())
    }

    /// Include uncached keyboards so their recovery panel also follows the fence.
    pub(super) fn disable_keys_config_events(&self) -> StateEvents {
        self.devices
            .records
            .iter()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.disable_keys)
            })
            .fold(StateEvents::none(), |events, record| {
                events.and(StateEvent::DisableKeysChanged(record.device_key()))
            })
    }

    /// Read the shared fence for the write gate and existing recovery controls.
    pub(crate) fn disable_keys_config_reconciliation(&self) -> &ConfigReconciliation {
        self.disable_keys_reads.reconciliation()
    }
}

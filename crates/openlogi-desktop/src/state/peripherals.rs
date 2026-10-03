//! Peripheral desired state shares the existing config save and IPC lifecycle.

use openlogi_core::{
    binding::Action,
    config::Config,
    peripheral::{
        CapabilityId, ControlId, PeripheralConfig, PeripheralError, PeripheralRecord,
        PeripheralSnapshot, SettingValue,
    },
};

use super::{AppState, StateEvent, StateEvents};
use crate::services::ipc::{ManagePeripheral, PeripheralOperation};

#[derive(Default)]
pub(super) struct PeripheralUi {
    next_request: u64,
    pending: Option<Pending>,
    error: Option<PeripheralError>,
}

struct Pending {
    id: u64,
    changes_config: bool,
}

impl AppState {
    pub(crate) fn peripheral_snapshot(&self) -> &PeripheralSnapshot {
        &self.agent.peripherals
    }
    pub(crate) fn peripheral_issue(&self) -> Option<&PeripheralError> {
        self.peripherals.error.as_ref()
    }
    pub(crate) fn peripheral_busy(&self) -> bool {
        self.peripherals.pending.is_some()
    }

    pub(crate) fn peripheral_rule(
        &self,
        record: &PeripheralRecord,
    ) -> Result<Option<&PeripheralConfig>, PeripheralError> {
        self.config.peripheral_rule(record)
    }

    pub(crate) fn commit_peripheral_binding(
        &mut self,
        record: &PeripheralRecord,
        capability: &CapabilityId,
        control: ControlId,
        action: Option<Action>,
    ) -> StateEvents {
        self.edit_peripheral(|config| {
            config.set_peripheral_binding(record, capability, control, action)
        })
    }

    pub(crate) fn commit_peripheral_enabled(
        &mut self,
        record: &PeripheralRecord,
        enabled: bool,
    ) -> StateEvents {
        self.edit_peripheral(|config| config.set_peripheral_enabled(record, enabled))
    }

    pub(crate) fn commit_peripheral_value(
        &mut self,
        record: &PeripheralRecord,
        capability: &CapabilityId,
        key: String,
        value: SettingValue,
    ) -> StateEvents {
        self.edit_peripheral(|config| config.set_peripheral_value(record, capability, key, value))
    }

    fn edit_peripheral(
        &mut self,
        edit: impl FnOnce(&mut Config) -> Result<(), PeripheralError>,
    ) -> StateEvents {
        self.peripherals.error = self.config.edit(edit).err();
        if self.peripherals.error.is_none() && !self.persist_and_reload("peripheral settings") {
            self.peripherals.error = Some(PeripheralError::ConfigWriteFailed(
                self.config.issue().unwrap_or_default().into(),
            ));
        }
        StateEvent::PeripheralsChanged.into()
    }

    pub(crate) fn manage_peripheral(&mut self, operation: PeripheralOperation) -> StateEvents {
        if self.peripherals.pending.is_some() {
            return StateEvents::none();
        }
        self.peripherals.next_request += 1;
        let id = self.peripherals.next_request;
        let changes_config = matches!(operation, PeripheralOperation::Plugin(_));
        self.peripherals.error = None;
        if self.send_ipc(ManagePeripheral { id, operation }) {
            self.peripherals.pending = Some(Pending { id, changes_config });
        } else {
            self.peripherals.error = Some(PeripheralError::DriverUnavailable(
                "agent is unreachable".into(),
            ));
        }
        StateEvent::PeripheralsChanged.into()
    }

    pub(crate) fn peripheral_command_finished(
        &mut self,
        id: u64,
        result: Result<(), PeripheralError>,
    ) -> StateEvents {
        if self
            .peripherals
            .pending
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return StateEvents::none();
        }
        let pending = self.peripherals.pending.take();
        self.peripherals.error = result.err();
        if pending.is_some_and(|pending| pending.changes_config) {
            match self.config.refresh() {
                Ok(()) => self.restore_config_projections(),
                Err(error) => self.peripherals.error = Some(error),
            }
        }
        StateEvents::from(StateEvent::PeripheralsChanged).and(StateEvent::SettingsChanged)
    }
}

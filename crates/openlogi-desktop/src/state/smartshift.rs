//! SmartShift optimistic writes and post-write confirmation. Device reads are
//! swr-backed queries owned by the device-read service.

use gpui::{App, Context};
use openlogi_core::hid::{DeviceRoute, SmartShiftStatus};
use tracing::debug;

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::load::SmartShiftLoad;
use super::{AppState, SmartShiftWriteStatus, StateEvent};

/// One device's SmartShift write lifecycle.
///
/// A queued confirmation and an in-flight one are different states: only the
/// latter may accept a read tagged with its request id. Keeping that distinction
/// here also makes resetting a write after a route change atomic.
#[derive(Debug, Default)]
pub(super) enum SmartShiftDeviceState {
    #[default]
    Idle,
    Queued {
        expected: SmartShiftStatus,
        write_id: u64,
    },
    Confirming {
        expected: SmartShiftStatus,
        write_id: u64,
    },
    Confirmed,
    Failed,
}

impl SmartShiftDeviceState {
    fn status(&self) -> Option<SmartShiftWriteStatus> {
        match *self {
            Self::Idle => None,
            Self::Queued { expected, write_id } | Self::Confirming { expected, write_id } => {
                Some(SmartShiftWriteStatus::Applying { expected, write_id })
            }
            Self::Confirmed => Some(SmartShiftWriteStatus::Confirmed),
            Self::Failed => Some(SmartShiftWriteStatus::Failed),
        }
    }

    pub(super) fn queue(&mut self, expected: SmartShiftStatus, write_id: u64) {
        *self = Self::Queued { expected, write_id };
    }

    pub(super) fn begin_confirmation(&mut self) -> Option<u64> {
        let Self::Queued { expected, write_id } = *self else {
            return None;
        };
        *self = Self::Confirming { expected, write_id };
        Some(write_id)
    }

    fn confirming_expected(&self, write_id: u64) -> Option<SmartShiftStatus> {
        match *self {
            Self::Confirming {
                expected,
                write_id: current,
            } if current == write_id => Some(expected),
            Self::Idle
            | Self::Queued { .. }
            | Self::Confirming { .. }
            | Self::Confirmed
            | Self::Failed => None,
        }
    }

    fn fail_confirmation(&mut self, write_id: u64) {
        if matches!(
            self,
            Self::Confirming {
                write_id: current,
                ..
            } if *current == write_id
        ) {
            *self = Self::Failed;
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::Idle;
    }
}

impl AppState {
    pub(super) fn load_current_smartshift(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_smartshift(key.clone(), route, self.ipc_sender(), cx);
        self.apply_smartshift_read(&key, None);
    }

    pub(super) fn confirm_current_smartshift(&mut self, cx: &mut Context<Self>) {
        let Some((key, route, write_id)) = self.take_active_smartshift_confirm() else {
            return;
        };
        if !self.pointer.reads.confirm_smartshift(
            key.clone(),
            route,
            write_id,
            self.ipc_sender(),
            cx,
        ) {
            self.fail_smartshift_confirm(&key, write_id);
        }
    }

    pub(crate) fn retry_smartshift_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.retry_smartshift(&key);
            cx.emit(StateEvent::SmartShiftChanged(key));
        });
    }

    pub(crate) fn update_smartshift(cx: &mut App, status: SmartShiftStatus) {
        Self::update(cx, |state, cx| {
            let key = state.current_record().map(DeviceRecord::device_key);
            state.commit_smartshift(status);
            state.confirm_current_smartshift(cx);
            if let Some(key) = key {
                cx.emit(StateEvent::SmartShiftChanged(key));
            }
        });
    }

    /// The active device's resolved SmartShift config, if the read succeeded.
    /// Callers use it to preserve fields they don't mean to change (e.g.
    /// tunable torque) when writing back.
    #[must_use]
    pub fn current_smartshift_ready(&self) -> Option<SmartShiftStatus> {
        self.current_record()
            .and_then(|record| self.pointer.reads.smartshift_load(&record.device_key()))
            .and_then(|status| match status {
                SmartShiftLoad::Ready(s) => Some(**s),
                SmartShiftLoad::Unknown
                | SmartShiftLoad::Loading
                | SmartShiftLoad::Failed(_)
                | SmartShiftLoad::Unsupported(_) => None,
            })
    }

    pub(crate) fn smartshift_status_for(&self, key: &DeviceKey) -> SmartShiftLoad {
        self.pointer.reads.smartshift_status(key)
    }

    /// Post-write confirmation status for the active device.
    #[must_use]
    pub fn current_smartshift_write_status(&self) -> Option<SmartShiftWriteStatus> {
        self.current_record().and_then(|record| {
            self.devices
                .runtime
                .get(&record.device_key())
                .and_then(|entry| entry.smartshift.status())
        })
    }
    /// Drop `key`'s recorded SmartShift status so the caller can re-run
    /// discovery, and clear any post-write confirmation banner along with it.
    /// Backs the "click to retry" affordance on a [`SmartShiftLoad::Failed`]
    /// device and on a failed write confirmation.
    pub fn retry_smartshift(&mut self, key: &DeviceKey) {
        self.pointer.reads.retry_smartshift(key);
        if let Some(entry) = self.devices.runtime.get_mut(key) {
            entry.smartshift.reset();
        }
    }
    /// Apply a settled query to write-confirmation state if it still belongs to
    /// the current write. The service's generation guard independently rejects
    /// callbacks from queries replaced by a newer confirmation.
    pub(crate) fn apply_smartshift_read(&mut self, key: &DeviceKey, write_id: Option<u64>) {
        let current_write = self.devices.runtime.get(key).map(|entry| &entry.smartshift);
        if !smartshift_read_is_current(write_id, current_write) {
            debug!(key = %key, ?write_id, "stale SmartShift read result ignored");
            return;
        }
        let Some(write_id) = write_id else {
            return;
        };
        let expected = self
            .devices
            .runtime
            .get(key)
            .and_then(|entry| entry.smartshift.confirming_expected(write_id));
        let Some(expected) = expected else {
            return;
        };
        if let Some(outcome) =
            smartshift_write_outcome(expected, self.pointer.reads.smartshift_load(key))
        {
            self.devices
                .runtime
                .entry(key.clone())
                .or_default()
                .smartshift = match outcome {
                ConfirmationOutcome::Confirmed => SmartShiftDeviceState::Confirmed,
                ConfirmationOutcome::Failed => SmartShiftDeviceState::Failed,
            };
        }
    }
    /// Write a full SmartShift configuration to the active device (best-effort,
    /// on a background thread), optimistically cache it, and persist it to
    /// `config.toml` — the values live in device RAM and reset on a power
    /// cycle (#189), so the agent re-applies them when the device reconnects.
    /// No-op when no device is selected.
    pub fn commit_smartshift(&mut self, status: SmartShiftStatus) {
        let Some(record) = self.current_record() else {
            debug!("no active device — SmartShift change ignored");
            return;
        };
        let key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        let can_confirm = route.is_some();
        if let Some(persistent_key) = persistent_key {
            self.config.edit(|config| {
                config.set_smartshift(
                    &persistent_key,
                    openlogi_core::config::SmartShift::from(status),
                );
            });
            if !self.persist_and_reload("SmartShift") {
                return;
            }
        }
        if let Some(route) = route {
            self.send_ipc(crate::services::ipc::Command::SetSmartShift(route, status));
        }
        // Reflect the write immediately so the panel doesn't flicker back to
        // the previous value before a re-read lands, but queue a confirming
        // re-read: the write is fire-and-forget, so a sleeping device that
        // rejected or timed it out would otherwise leave this optimistic value
        // showing as "applied" forever (Ready blocks any further read).
        let expected = status;
        self.pointer.reads.set_smartshift_ready(&key, expected);
        let write_id = can_confirm.then(|| {
            let write_id = self.pointer.next_smartshift_write_id;
            self.pointer.next_smartshift_write_id =
                self.pointer.next_smartshift_write_id.saturating_add(1);
            self.devices
                .runtime
                .entry(key.clone())
                .or_default()
                .smartshift
                .queue(expected, write_id);
            write_id
        });
        if write_id.is_none() {
            self.devices.runtime.entry(key).or_default().smartshift = SmartShiftDeviceState::Failed;
        }
    }
    /// Take the active device's pending SmartShift confirm, if any. Returns
    /// the `(device key, route, write_id)` for a one-shot re-read that
    /// replaces the optimistic value with the device's real state; consumed
    /// once so it doesn't re-fire.
    pub fn take_active_smartshift_confirm(&mut self) -> Option<(DeviceKey, DeviceRoute, u64)> {
        let record = self.current_record()?;
        let key = record.device_key();
        let route = record.route.clone()?;
        let write_id = self
            .devices
            .runtime
            .get_mut(&key)?
            .smartshift
            .begin_confirmation()?;
        Some((key, route, write_id))
    }
    /// Mark a post-write confirmation as failed when its reply channel closes.
    pub fn fail_smartshift_confirm(&mut self, key: &DeviceKey, write_id: u64) {
        if let Some(entry) = self.devices.runtime.get_mut(key) {
            entry.smartshift.fail_confirmation(write_id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmationOutcome {
    Confirmed,
    Failed,
}

pub(crate) fn smartshift_write_outcome(
    expected: SmartShiftStatus,
    load: Option<&SmartShiftLoad>,
) -> Option<ConfirmationOutcome> {
    match load {
        Some(SmartShiftLoad::Ready(actual)) if **actual == expected => {
            Some(ConfirmationOutcome::Confirmed)
        }
        Some(SmartShiftLoad::Ready(_)) => Some(ConfirmationOutcome::Failed),
        Some(SmartShiftLoad::Failed(_) | SmartShiftLoad::Unsupported(_)) => {
            Some(ConfirmationOutcome::Failed)
        }
        None | Some(SmartShiftLoad::Unknown | SmartShiftLoad::Loading) => None,
    }
}

pub(crate) fn smartshift_read_is_current(
    read_id: Option<u64>,
    write: Option<&SmartShiftDeviceState>,
) -> bool {
    match (read_id, write) {
        (
            Some(read_id),
            Some(SmartShiftDeviceState::Confirming {
                write_id: current, ..
            }),
        ) => read_id == *current,
        (
            None,
            Some(SmartShiftDeviceState::Queued { .. } | SmartShiftDeviceState::Confirming { .. }),
        )
        | (Some(_), _) => false,
        (None, _) => true,
    }
}

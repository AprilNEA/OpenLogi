//! SmartShift optimistic writes and post-write confirmation. The lazy read
//! cache itself lives in [`super::load::LazyDeviceData`], reached directly as
//! `self.reads.smartshift`.

use openlogi_core::hid::{DeviceRoute, SmartShiftMode, SmartShiftStatus, WriteError};
use tracing::debug;

use super::device_key::DeviceKey;
use super::load::SmartShiftLoad;
use super::{AppState, SmartShiftWriteStatus};

impl AppState {
    /// The active device's resolved SmartShift config, if the read succeeded.
    /// Callers use it to preserve fields they don't mean to change (e.g.
    /// tunable torque) when writing back.
    #[must_use]
    pub fn current_smartshift_ready(&self) -> Option<SmartShiftStatus> {
        self.current_record()
            .and_then(|record| self.reads.smartshift.get(&record.device_key()))
            .and_then(|status| match status {
                SmartShiftLoad::Ready(s) => Some(*s),
                SmartShiftLoad::Unknown
                | SmartShiftLoad::Loading
                | SmartShiftLoad::Failed(_)
                | SmartShiftLoad::Unsupported(_) => None,
            })
    }
    /// Post-write confirmation status for the active device.
    #[must_use]
    pub fn current_smartshift_write_status(&self) -> Option<SmartShiftWriteStatus> {
        self.current_record().and_then(|record| {
            self.device_ui
                .get(&record.device_key())
                .and_then(|entry| entry.smartshift_write_status)
        })
    }
    /// Drop `key`'s recorded SmartShift status so the next render re-runs
    /// discovery, and clear any post-write confirmation banner along with it.
    /// Backs the "click to retry" affordance on a [`SmartShiftLoad::Failed`]
    /// device and on a failed write confirmation.
    pub fn retry_smartshift(&mut self, key: &DeviceKey) {
        self.reads.smartshift.retry(key);
        if let Some(entry) = self.device_ui.get_mut(key) {
            entry.smartshift_write_status = None;
        }
    }
    /// Store a SmartShift read result if it still matches the known device
    /// route and write identity, with the same transient-retry /
    /// permanent-unsupported handling as [`Self::store_dpi_info`].
    pub fn store_smartshift_status(
        &mut self,
        key: DeviceKey,
        route: &DeviceRoute,
        write_id: Option<u64>,
        result: Result<SmartShiftStatus, WriteError>,
    ) {
        let current_write_status = self
            .device_ui
            .get(&key)
            .and_then(|entry| entry.smartshift_write_status);
        if !smartshift_read_is_current(write_id, current_write_status.as_ref()) {
            debug!(key = %key, ?write_id, "stale SmartShift read result ignored");
            return;
        }
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key);
        self.reads.smartshift.store(
            key.clone(),
            result,
            smartshift_error_is_permanent,
            matches_route,
            still_present,
            "SmartShift",
        );
        let expected = match self
            .device_ui
            .get(&key)
            .and_then(|entry| entry.smartshift_write_status)
        {
            Some(SmartShiftWriteStatus::Applying { expected, .. }) => Some(expected),
            Some(SmartShiftWriteStatus::Confirmed | SmartShiftWriteStatus::Failed) | None => None,
        };
        if let Some(status) = expected.and_then(|expected| {
            smartshift_write_outcome(expected, self.reads.smartshift.get(&key))
        }) {
            self.device_ui
                .entry(key)
                .or_default()
                .smartshift_write_status = Some(status);
        }
    }
    /// Write a full SmartShift configuration to the active device (best-effort,
    /// on a background thread), optimistically cache it, and persist it to
    /// `config.toml` — the values live in device RAM and reset on a power
    /// cycle (#189), so the agent re-applies them when the device reconnects.
    /// No-op when no device is selected.
    pub fn commit_smartshift(
        &mut self,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) {
        let Some(record) = self.current_record() else {
            debug!("no active device — SmartShift change ignored");
            return;
        };
        let key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        let can_confirm = route.is_some();
        if let Some(persistent_key) = persistent_key {
            self.config.set_smartshift(
                &persistent_key,
                openlogi_core::config::SmartShift {
                    mode: mode.into(),
                    auto_disengage,
                    tunable_torque,
                },
            );
            if !self.persist_and_reload("SmartShift") {
                return;
            }
        }
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetSmartShift(
                route,
                mode,
                auto_disengage,
                tunable_torque,
            ));
        }
        // Reflect the write immediately so the panel doesn't flicker back to
        // the previous value before a re-read lands, but queue a confirming
        // re-read: the write is fire-and-forget, so a sleeping device that
        // rejected or timed it out would otherwise leave this optimistic value
        // showing as "applied" forever (Ready blocks any further read).
        let expected = SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        };
        self.reads.smartshift.set_ready(key.clone(), expected);
        let write_id = can_confirm.then(|| {
            let write_id = self.next_smartshift_write_id;
            self.next_smartshift_write_id = self.next_smartshift_write_id.saturating_add(1);
            self.device_ui
                .entry(key.clone())
                .or_default()
                .smartshift_pending_confirm = Some(write_id);
            write_id
        });
        self.device_ui
            .entry(key)
            .or_default()
            .smartshift_write_status = Some(match write_id {
            Some(write_id) => SmartShiftWriteStatus::Applying { expected, write_id },
            None => SmartShiftWriteStatus::Failed,
        });
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
            .device_ui
            .get_mut(&key)?
            .smartshift_pending_confirm
            .take()?;
        Some((key, route, write_id))
    }
    /// Mark a post-write confirmation as failed when its reply channel closes.
    pub fn fail_smartshift_confirm(&mut self, key: &DeviceKey, write_id: u64) {
        if let Some(entry) = self.device_ui.get_mut(key)
            && matches!(
                entry.smartshift_write_status,
                Some(SmartShiftWriteStatus::Applying {
                    write_id: current,
                    ..
                }) if current == write_id
            )
        {
            entry.smartshift_write_status = Some(SmartShiftWriteStatus::Failed);
        }
    }
}

pub(crate) fn smartshift_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

pub(crate) fn smartshift_write_outcome(
    expected: SmartShiftStatus,
    load: Option<&SmartShiftLoad>,
) -> Option<SmartShiftWriteStatus> {
    match load {
        Some(SmartShiftLoad::Ready(actual)) if *actual == expected => {
            Some(SmartShiftWriteStatus::Confirmed)
        }
        Some(SmartShiftLoad::Ready(_)) => Some(SmartShiftWriteStatus::Failed),
        Some(SmartShiftLoad::Failed(_) | SmartShiftLoad::Unsupported(_)) => {
            Some(SmartShiftWriteStatus::Failed)
        }
        None | Some(SmartShiftLoad::Unknown | SmartShiftLoad::Loading) => None,
    }
}

pub(crate) fn smartshift_read_is_current(
    read_id: Option<u64>,
    write_status: Option<&SmartShiftWriteStatus>,
) -> bool {
    match (read_id, write_status) {
        (
            Some(read_id),
            Some(SmartShiftWriteStatus::Applying {
                write_id: current, ..
            }),
        ) => read_id == *current,
        (None, Some(SmartShiftWriteStatus::Applying { .. })) | (Some(_), _) => false,
        (None, _) => true,
    }
}

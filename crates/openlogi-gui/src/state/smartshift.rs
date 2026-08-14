//! SmartShift load state, optimistic writes, and confirmation.

use openlogi_hid::{DeviceRoute, SmartShiftMode, SmartShiftStatus, WriteError};
use tracing::debug;

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::load::SmartShiftLoad;
use super::{AppState, SmartShiftWriteStatus};

impl AppState {
    /// SmartShift configuration status for the active device.
    #[must_use]
    pub fn current_smartshift_status(&self) -> SmartShiftLoad {
        self.current_record()
            .map_or(SmartShiftLoad::Unknown, |record| {
                self.smartshift_data.status(&record.device_key())
            })
    }
    /// Whether the active device still needs a SmartShift read (no status
    /// recorded). Cheaper than comparing a cloned [`SmartShiftLoad`] on the
    /// per-frame render path.
    #[must_use]
    pub fn current_smartshift_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.smartshift_data.unqueried(&record.device_key()))
    }
    /// The active device's resolved SmartShift config, if the read succeeded.
    /// Callers use it to preserve fields they don't mean to change (e.g.
    /// tunable torque) when writing back.
    #[must_use]
    pub fn current_smartshift_ready(&self) -> Option<SmartShiftStatus> {
        self.current_record()
            .and_then(|record| self.smartshift_data.get(&record.device_key()))
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
            self.smartshift_write_status
                .get(&record.config_key)
                .copied()
        })
    }
    /// Mark SmartShift discovery as in flight for `key`.
    pub fn mark_smartshift_loading(&mut self, key: &DeviceKey) {
        self.smartshift_data.mark_loading(key);
    }
    /// Reset a stuck `Loading` for `key` back to `Unknown` — called when the
    /// read worker vanished without delivering a result.
    pub fn clear_smartshift_loading(&mut self, key: &DeviceKey) {
        self.smartshift_data.clear_loading(key);
    }
    /// Drop the active device's recorded SmartShift status so the next render
    /// re-runs discovery. Backs the "click to retry" affordance on a
    /// [`SmartShiftLoad::Failed`] device.
    pub fn retry_active_smartshift(&mut self) {
        if let Some(key) = self.current_record().map(DeviceRecord::device_key) {
            self.smartshift_data.retry(&key);
            self.smartshift_write_status.remove(key.as_str());
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
        if !smartshift_read_is_current(write_id, self.smartshift_write_status.get(key.as_str())) {
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
        let status_key = key.to_string();
        self.smartshift_data.store(
            key,
            result,
            smartshift_error_is_permanent,
            matches_route,
            still_present,
            "SmartShift",
        );
        let expected = match self.smartshift_write_status.get(&status_key) {
            Some(SmartShiftWriteStatus::Applying { expected, .. }) => Some(*expected),
            Some(SmartShiftWriteStatus::Confirmed | SmartShiftWriteStatus::Failed) | None => None,
        };
        if let Some(status) = expected.and_then(|expected| {
            smartshift_write_outcome(
                expected,
                self.smartshift_data
                    .get(&DeviceKey::from(status_key.as_str())),
            )
        }) {
            self.smartshift_write_status.insert(status_key, status);
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
        let key = record.config_key.clone();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        let can_confirm = route.is_some();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetSmartShift(
                route,
                mode,
                auto_disengage,
                tunable_torque,
            ));
        }
        if let Some(persistent_key) = persistent_key {
            self.config.set_smartshift(
                &persistent_key,
                openlogi_core::config::SmartShift {
                    mode: mode.into(),
                    auto_disengage,
                    tunable_torque,
                },
            );
            self.persist_and_reload("SmartShift");
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
        self.smartshift_data
            .set_ready(DeviceKey::from(key.as_str()), expected);
        let write_id = can_confirm.then(|| {
            let write_id = self.next_smartshift_write_id;
            self.next_smartshift_write_id = self.next_smartshift_write_id.saturating_add(1);
            self.smartshift_pending_confirm
                .insert(key.clone(), write_id);
            write_id
        });
        self.smartshift_write_status.insert(
            key,
            match write_id {
                Some(write_id) => SmartShiftWriteStatus::Applying { expected, write_id },
                None => SmartShiftWriteStatus::Failed,
            },
        );
    }
    /// Take the active device's pending SmartShift confirm, if any. Returns the
    /// `(config_key, route, write_id)` for a one-shot re-read that replaces the
    /// optimistic value with the device's real state; consumed once so it
    /// doesn't re-fire.
    pub fn take_active_smartshift_confirm(&mut self) -> Option<(DeviceKey, DeviceRoute, u64)> {
        let record = self.current_record()?;
        let key = record.device_key();
        let route = record.route.clone()?;
        self.smartshift_pending_confirm
            .remove(key.as_str())
            .map(|write_id| (key, route, write_id))
    }
    /// Mark a post-write confirmation as failed when its reply channel closes.
    pub fn fail_smartshift_confirm(&mut self, key: &DeviceKey, write_id: u64) {
        if matches!(
            self.smartshift_write_status.get(key.as_str()),
            Some(SmartShiftWriteStatus::Applying {
                write_id: current,
                ..
            }) if *current == write_id
        ) {
            self.smartshift_write_status
                .insert(key.to_string(), SmartShiftWriteStatus::Failed);
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

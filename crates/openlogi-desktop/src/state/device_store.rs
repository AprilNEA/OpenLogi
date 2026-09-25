//! Device catalog, active selection, and per-device session rows.

use std::collections::BTreeMap;

use super::device_key::DeviceKey;
use super::device_session::DeviceSession;
use super::devices::DeviceRecord;

/// Owns the merged device catalog and keeps its active index valid.
pub(super) struct DeviceStore {
    selected: Option<usize>,
    pub(super) records: Vec<DeviceRecord>,
    pub(super) sessions: BTreeMap<DeviceKey, DeviceSession>,
}

impl DeviceStore {
    pub(super) fn new(records: Vec<DeviceRecord>, selected: usize) -> Self {
        let selected = (!records.is_empty()).then(|| selected.min(records.len() - 1));
        Self {
            selected,
            records,
            sessions: BTreeMap::new(),
        }
    }

    pub(super) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub(super) fn current(&self) -> Option<&DeviceRecord> {
        self.selected.and_then(|index| self.records.get(index))
    }

    pub(super) fn select(&mut self, index: usize) -> bool {
        if index >= self.records.len() || self.selected == Some(index) {
            return false;
        }
        self.selected = Some(index);
        true
    }

    pub(super) fn replace(&mut self, records: Vec<DeviceRecord>, selected: usize) {
        self.selected = (!records.is_empty()).then(|| selected.min(records.len() - 1));
        self.records = records;

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        for record in &self.records {
            if record.battery.is_some() {
                self.sessions
                    .entry(record.device_key())
                    .or_default()
                    .battery_model = Some(record.model_key.clone());
            } else if (record.online || record.route.is_some())
                && let Some(session) = self.sessions.get_mut(record.config_key.as_str())
                && session.battery_model.as_deref() != Some(record.model_key.as_str())
            {
                // Synthetic offline placeholders may substitute the config key
                // for the model; they cannot establish replacement evidence.
                session.battery_model = None;
            }
        }
    }
}

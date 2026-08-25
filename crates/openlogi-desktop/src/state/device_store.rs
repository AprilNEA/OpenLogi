//! Device catalog, active selection, and per-device runtime rows.

use std::collections::BTreeMap;

use super::device_key::DeviceKey;
use super::device_runtime::DeviceRuntimeState;
use super::devices::DeviceRecord;

/// Owns the merged device catalog and keeps its active index valid.
pub(super) struct DeviceStore {
    selected: Option<usize>,
    pub(super) records: Vec<DeviceRecord>,
    pub(super) runtime: BTreeMap<DeviceKey, DeviceRuntimeState>,
}

impl DeviceStore {
    pub(super) fn new(records: Vec<DeviceRecord>, selected: usize) -> Self {
        let selected = (!records.is_empty()).then(|| selected.min(records.len() - 1));
        Self {
            selected,
            records,
            runtime: BTreeMap::new(),
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
    }
}

//! Device catalog, active selection, and per-device session rows.

use std::collections::BTreeMap;

use openlogi_core::hid::{DeviceRoute, ReceiverProtocol};

use super::device_key::DeviceKey;
use super::device_session::{DeviceSession, ObservedConnection};
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
        let mut store = Self {
            selected,
            records,
            sessions: BTreeMap::new(),
        };
        store.observe_connections();
        store
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
        self.observe_connections();

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        for record in &self.records {
            if record.battery.is_some() {
                self.sessions
                    .entry(record.device_key())
                    .or_default()
                    .battery_model = Some(record.model_key.clone());
            } else if let Some(session) = self.sessions.get_mut(record.config_key.as_str())
                && session.battery_model.as_deref() != Some(record.model_key.as_str())
            {
                session.battery_model = None;
            }
        }
    }

    fn observe_connections(&mut self) {
        for record in self.records.iter().filter(|record| record.online) {
            let transport = match record.route.as_ref() {
                Some(DeviceRoute::Direct { product_id, .. }) => record
                    .model_info
                    .as_ref()
                    .and_then(|model| model.transport_for_product_id(*product_id))
                    .map(ObservedConnection::Direct),
                Some(DeviceRoute::Bolt { .. }) => {
                    Some(ObservedConnection::Receiver(ReceiverProtocol::Bolt))
                }
                Some(DeviceRoute::Unifying { .. }) => {
                    Some(ObservedConnection::Receiver(ReceiverProtocol::Unifying))
                }
                _ => None,
            };
            // An incomplete probe is not evidence of a connection-mode change.
            if let Some(transport) = transport {
                self.sessions
                    .entry(record.device_key())
                    .or_default()
                    .last_connection = Some(transport);
            }
        }
    }
}

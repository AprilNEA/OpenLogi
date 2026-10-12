//! Sidetone (`0x0604`) state management for headsets.

use gpui::Context;
use openlogi_core::hid::WriteError;

use super::events::StateEvents;
use super::{AppState, DeviceKey, SidetoneLoad, StateEvent};

impl AppState {
    /// Whether the active device announces the sidetone feature.
    #[must_use]
    pub fn current_sidetone_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.sidetone)
    }

    /// The active device's current sidetone load state.
    #[must_use]
    pub fn current_sidetone_load(&self) -> SidetoneLoad {
        let Some(record) = self.current_record() else {
            return SidetoneLoad::Unknown;
        };
        let key = record.device_key();
        if let Some(load) = self.pointer.reads.sidetone_load(&key) {
            return load.clone();
        }
        if let Some(session) = self.devices.sessions.get(&key)
            && let Some(level) = session.sidetone
        {
            return SidetoneLoad::Ready(std::sync::Arc::new(level));
        }
        SidetoneLoad::Unknown
    }

    /// What sidetone slider level to display right now (if known).
    #[must_use]
    #[cfg(test)]
    pub fn current_sidetone_shown(&self) -> Option<u8> {
        match self.current_sidetone_load() {
            SidetoneLoad::Ready(level) => Some(*level),
            SidetoneLoad::Unknown
            | SidetoneLoad::Loading
            | SidetoneLoad::Failed(_)
            | SidetoneLoad::Unsupported(_) => None,
        }
    }

    /// Commit a user-chosen sidetone level to the headset over IPC.
    ///
    /// The slider shows `level` at once; if the write fails or the device
    /// rejects it, [`Self::apply_sidetone_written`] triggers a re-read or surfaces failure.
    pub fn commit_sidetone(&mut self, level: u8) -> StateEvents {
        if !self.current_sidetone_supported() {
            return StateEvents::none();
        }
        let events = self.for_current_device(StateEvent::SidetoneChanged);
        let Some((key, route)) = self
            .current_record()
            .and_then(|r| Some((r.device_key(), r.route.clone()?)))
        else {
            return StateEvents::none();
        };

        self.devices
            .sessions
            .entry(key.clone())
            .or_default()
            .sidetone = Some(level);
        self.pointer.reads.set_sidetone_ready(&key, level);
        self.send_ipc(crate::services::ipc::SetSidetone { key, route, level });
        events
    }

    /// The headset's answer to a [`Self::commit_sidetone`] write.
    pub fn apply_sidetone_written(
        &mut self,
        key: &DeviceKey,
        _level: u8,
        result: &Result<(), WriteError>,
    ) -> StateEvents {
        if result.is_ok() {
            return StateEvents::none();
        }
        // Write failed or rejected: clear optimistic level and re-query device
        // so slider does not stay on a rejected optimistic value.
        if let Some(session) = self.devices.sessions.get_mut(key) {
            session.sidetone = None;
        }
        self.pointer.reads.refresh_sidetone(key);
        StateEvent::SidetoneChanged(key.clone()).into()
    }

    /// Retry an exhausted sidetone read for the active headset.
    pub fn retry_current_sidetone(&mut self) -> StateEvents {
        let Some(key) = self
            .current_record()
            .map(super::devices::DeviceRecord::device_key)
        else {
            return StateEvents::none();
        };
        self.pointer.reads.retry_sidetone(&key);
        StateEvent::SidetoneChanged(key).into()
    }

    /// Start background query for current headset sidetone.
    pub(super) fn load_current_sidetone(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.sidetone)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_sidetone(key, route, self.ipc_sender(), cx);
    }
}

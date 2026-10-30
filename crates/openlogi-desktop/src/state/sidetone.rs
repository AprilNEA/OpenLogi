//! Headset sidetone: the level the headset plays the microphone back at.
//!
//! The headset stores the level itself and keeps it across power cycles, so
//! nothing is persisted here. The reading is the swr-backed device query the
//! device-read service owns, and the slider shows it.
//!
//! A release of the slider writes the headset. Its answer is the level the
//! headset confirms. A refusal reads the headset again, so the slider returns
//! to the headset's own level; as for a mouse with no saved pointer speed,
//! nothing else needs marking.

use openlogi_core::hid::{SidetoneLevel, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::SidetoneLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device is a headset with HID++ `0x8300` sidetone.
    #[must_use]
    pub fn current_sidetone_supported(&self) -> bool {
        self.current_record().is_some_and(has_sidetone)
    }

    /// What is known of the active headset's sidetone reading.
    #[must_use]
    pub fn current_sidetone_load(&self) -> SidetoneLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.sidetone_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// Write `level` to the active headset through the agent. The slider
    /// shows `level` at once; the headset's confirmation arrives as
    /// [`Self::apply_sidetone_written`]. No-op when no device is selected,
    /// it has no `0x8300`, or it is offline.
    pub fn commit_sidetone(&mut self, level: SidetoneLevel) -> StateEvents {
        let events = self.for_current_device(StateEvent::SidetoneChanged);
        let Some((key, route)) = self
            .current_record()
            .filter(|record| has_sidetone(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return events;
        };
        self.pointer.reads.set_sidetone_ready(&key, level);
        self.send_ipc(crate::services::ipc::SetSidetone { route, level, key });
        events
    }

    /// The headset's answer to a [`Self::commit_sidetone`] write: show the
    /// confirmed level, or — when the headset refused or could not be
    /// reached — read it again so the optimistic value does not stand in for
    /// its own.
    pub fn apply_sidetone_written(
        &mut self,
        key: &DeviceKey,
        result: Result<SidetoneLevel, WriteError>,
    ) -> StateEvents {
        match result {
            Ok(level) => self.pointer.reads.set_sidetone_ready(key, level),
            Err(error) => {
                warn!(%error, key = %key, "sidetone write did not land — re-reading the headset");
                self.pointer.reads.refresh_sidetone(key);
            }
        }
        StateEvent::SidetoneChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted sidetone read — the "click to retry"
    /// affordance on a [`SidetoneLoad::Failed`] headset.
    pub(crate) fn retry_sidetone_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_sidetone(key);
        StateEvent::SidetoneChanged(key.clone()).into()
    }

    /// `key`'s headset came back online: read it again in case another host
    /// changed its level.
    pub(super) fn sidetone_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.reads.refresh_sidetone(key);
    }

    pub(super) fn load_current_sidetone(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| has_sidetone(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the headset's sidetone");
        self.pointer
            .reads
            .ensure_sidetone(key, route, self.ipc_sender(), cx);
    }
}

fn has_sidetone(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.sidetone)
}

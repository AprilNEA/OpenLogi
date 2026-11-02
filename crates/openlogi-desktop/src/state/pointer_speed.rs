//! Mouse pointer speed: the persisted preference and the mouse's own reading.
//!
//! The *setting* is `config.toml`'s per-device `pointer_speed`. The agent
//! writes it over HID++ `0x2205` when the mouse connects. The *reading* is
//! the swr-backed device query the device-read service owns: the speed the
//! mouse moves at now. The two differ while precision mode is on, because
//! the agent then moves the pointer at [`PointerSpeed::MIN`] and keeps the
//! setting. So the slider shows the setting, and shows the reading only for
//! a mouse with no saved speed.
//!
//! A release of the slider persists the setting and writes the mouse in one
//! request. Its answer is the mouse's read-back, and the write ends precision
//! mode on that mouse. A refusal sends the query back to the mouse and marks
//! the mouse refused, because the slider keeps showing the saved setting. The
//! mark stands until the next write lands, a reading matches the setting, or
//! the mouse reconnects: the agent then writes the saved setting again, and
//! the reading is asked again.

use openlogi_core::hid::{PointerSpeed, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::PointerSpeedLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active mouse scales its pointer with HID++ `0x2205`
    /// instead of adjustable DPI.
    #[must_use]
    pub fn current_pointer_speed_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.pointer_scaling)
    }

    /// What is known of the active mouse's pointer-speed reading.
    #[must_use]
    pub fn current_pointer_speed_load(&self) -> PointerSpeedLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.pointer_speed_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The speed the slider shows for the active mouse: its saved speed, or
    /// its reading when nothing is saved. `None` while it has neither.
    #[must_use]
    pub fn current_pointer_speed(&self) -> Option<PointerSpeed> {
        let record = self.current_record()?;
        record
            .persistent_config_key()
            .and_then(|key| self.config.pointer_speed(key))
            .or_else(|| match self.current_pointer_speed_load() {
                PointerSpeedLoad::Ready(speed) => Some(*speed),
                PointerSpeedLoad::Unknown
                | PointerSpeedLoad::Loading
                | PointerSpeedLoad::Failed(_)
                | PointerSpeedLoad::Unsupported(_) => None,
            })
    }

    /// Whether the active mouse refused its last pointer-speed write and does
    /// not read the saved speed yet. The slider then shows a speed the mouse
    /// does not move at. The agent writes the saved speed again when the
    /// mouse reconnects.
    #[must_use]
    pub fn current_pointer_speed_refused(&self) -> bool {
        let Some(record) = self.current_record() else {
            return false;
        };
        if !self
            .pointer
            .refused_pointer_speeds
            .contains(&record.device_key())
        {
            return false;
        }
        let Some(saved) = record
            .persistent_config_key()
            .and_then(|key| self.config.pointer_speed(key))
        else {
            // With nothing saved the slider shows the reading itself.
            return false;
        };
        match self.current_pointer_speed_load() {
            PointerSpeedLoad::Ready(reading) => *reading != saved,
            PointerSpeedLoad::Unknown
            | PointerSpeedLoad::Loading
            | PointerSpeedLoad::Failed(_)
            | PointerSpeedLoad::Unsupported(_) => true,
        }
    }

    /// Persist `speed` for the active mouse and write it through the agent.
    /// The slider shows `speed` at once; the mouse's read-back arrives as
    /// [`Self::apply_pointer_speed_written`]. No-op when no device is
    /// selected or it has no `0x2205`.
    pub fn commit_pointer_speed(&mut self, speed: PointerSpeed) -> StateEvents {
        let events = self.for_current_device(StateEvent::PointerSpeedChanged);
        if !self.current_pointer_speed_supported() {
            return events;
        }
        let Some(record) = self.current_record() else {
            return events;
        };
        let device_key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(persistent_key) = persistent_key {
            self.config
                .edit(|config| config.set_pointer_speed(&persistent_key, speed));
            if !self.persist_and_reload("pointer speed") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        self.pointer.refused_pointer_speeds.remove(&device_key);
        self.pointer
            .reads
            .set_pointer_speed_ready(&device_key, speed);
        self.send_ipc(crate::services::ipc::SetPointerSpeed {
            route,
            speed,
            key: device_key,
        });
        events
    }

    /// The mouse's answer to a [`Self::commit_pointer_speed`] write: show the
    /// read-back, or — when the mouse refused or could not be reached — ask
    /// it again so the optimistic value does not stand in for its own, and
    /// mark the mouse refused for [`Self::current_pointer_speed_refused`].
    pub fn apply_pointer_speed_written(
        &mut self,
        key: &DeviceKey,
        result: Result<PointerSpeed, WriteError>,
    ) -> StateEvents {
        match result {
            Ok(speed) => {
                self.pointer.refused_pointer_speeds.remove(key);
                self.pointer.reads.set_pointer_speed_ready(key, speed);
            }
            Err(error) => {
                warn!(%error, key = %key, "pointer-speed write did not land — re-reading the mouse");
                self.pointer.refused_pointer_speeds.insert(key.clone());
                self.pointer.reads.refresh_pointer_speed(key);
            }
        }
        StateEvent::PointerSpeedChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted pointer-speed read — the "click to retry"
    /// affordance on a [`PointerSpeedLoad::Failed`] mouse.
    pub(crate) fn retry_pointer_speed_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_pointer_speed(key);
        StateEvent::PointerSpeedChanged(key.clone()).into()
    }

    /// `key`'s mouse came back online. The agent writes the saved speed
    /// again when a mouse connects, so a refused write no longer stands, and
    /// the mouse is read again so the slider follows it.
    pub(super) fn pointer_speed_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.refused_pointer_speeds.remove(key);
        self.pointer.reads.refresh_pointer_speed(key);
    }

    pub(super) fn load_current_pointer_speed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| scales_pointer(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the mouse's pointer speed");
        self.pointer
            .reads
            .ensure_pointer_speed(key, route, self.ipc_sender(), cx);
    }
}

fn scales_pointer(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.pointer_scaling)
}

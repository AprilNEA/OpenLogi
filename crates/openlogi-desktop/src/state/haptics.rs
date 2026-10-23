//! Haptic feedback: the persisted preference and the device's own reading.
//!
//! Two facts, one authority each. The *setting* is `config.toml`'s per-device
//! `haptics`, which the agent re-applies when the device reconnects. The
//! *reading* is the swr-backed device query the device-read service owns: the
//! engine, power-save and intensity settings the device holds now, the
//! intensities it accepts, and its factory intensity.
//!
//! A change persists the setting and writes the device in one request whose
//! answer is the settings the device echoed, as for Fn-lock: the controls
//! show the asked-for settings at once, then the echo replaces them, or a
//! refusal sends the query back to the device for the truth.

use openlogi_core::hid::{HapticIntensity, HapticSettings, WriteError};
use tracing::warn;

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::HapticsLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device reports haptic feedback (HID++ `0x19b0`).
    #[must_use]
    pub fn current_haptics_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.haptic_feedback)
    }

    /// The persisted haptic settings for the active device, or `None` when
    /// the user never set them (the device keeps its own).
    #[must_use]
    pub fn current_haptics_setting(&self) -> Option<HapticSettings> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.haptics(key))
    }

    /// Whether the user silenced the active device's haptic engine in
    /// OpenLogi, which silences its Actions Ring too.
    #[must_use]
    pub fn current_haptics_silenced(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.haptics_silenced(key))
    }

    /// What is known of the active device's haptic reading.
    #[must_use]
    pub fn current_haptics_load(&self) -> HapticsLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.haptics_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The settings the controls show: the device's own reading when it has
    /// landed, else the persisted preference. `None` while there is neither.
    #[must_use]
    pub fn current_haptics_shown(&self) -> Option<HapticSettings> {
        match self.current_haptics_load() {
            HapticsLoad::Ready(reading) => Some(reading.settings),
            HapticsLoad::Unknown
            | HapticsLoad::Loading
            | HapticsLoad::Failed(_)
            | HapticsLoad::Unsupported(_) => self.current_haptics_setting(),
        }
    }

    /// Persist `settings` for the active device and write them through the
    /// agent; with `preview`, the device plays one short pulse at them. The
    /// controls show `settings` at once; the device's echo arrives as
    /// [`Self::apply_haptics_written`]. No-op when no device is selected or
    /// it reports no haptic feedback.
    ///
    /// `settings` carries every field, so a change to one control saves the
    /// others as shown. While an Actions Ring session borrows the engine the
    /// device reads back as on, and a slider change then saves the engine as
    /// on: the user saw it on when they chose the intensity.
    pub fn commit_haptics(&mut self, settings: HapticSettings, preview: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::HapticsChanged);
        if !self.current_haptics_supported() {
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
                .edit(|config| config.set_haptics(&persistent_key, settings));
            if !self.persist_and_reload("haptics") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        self.pointer.reads.set_haptics_ready(&device_key, settings);
        let write = self.pointer.next_haptics_write;
        self.pointer.next_haptics_write = write.saturating_add(1);
        self.pointer
            .pending_haptics_writes
            .insert(device_key.clone(), write);
        self.send_ipc(crate::services::ipc::SetHaptics {
            route,
            settings,
            preview,
            key: device_key,
            write,
        });
        events
    }

    /// Commit the intensity a drag of `key`'s slider was released on, with
    /// a preview pulse. A release after the selection moved to another device
    /// commits nothing: the dragged value belongs to `key`, and the other
    /// device's settings must not be combined with it.
    pub fn commit_haptics_intensity(
        &mut self,
        key: &DeviceKey,
        intensity: HapticIntensity,
    ) -> StateEvents {
        if !self.is_current_device(key) {
            return StateEvents::none();
        }
        self.current_haptics_shown()
            .map_or_else(StateEvents::none, |shown| {
                self.commit_haptics(HapticSettings { intensity, ..shown }, true)
            })
    }

    /// The device's answer to a [`Self::commit_haptics`] write: show the
    /// echoed settings, or — when the device refused or could not be reached
    /// — ask it again so the optimistic settings do not stand in for its own.
    ///
    /// Only the newest write's answer counts, as for DPI: an older one would
    /// show settings the user has since changed, and the next switch would
    /// write them back.
    pub fn apply_haptics_written(
        &mut self,
        key: &DeviceKey,
        write: u64,
        result: Result<HapticSettings, WriteError>,
    ) -> StateEvents {
        if self.pointer.pending_haptics_writes.get(key) != Some(&write) {
            return StateEvents::none();
        }
        self.pointer.pending_haptics_writes.remove(key);
        match result {
            Ok(settings) => self.pointer.reads.set_haptics_ready(key, settings),
            Err(error) => {
                warn!(%error, key = %key, "haptics write did not land — re-reading the device");
                self.pointer.reads.refresh_haptics(key);
            }
        }
        StateEvent::HapticsChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted haptics read — the "click to retry"
    /// affordance on a [`HapticsLoad::Failed`] device.
    pub(crate) fn retry_haptics_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_haptics(key);
        StateEvent::HapticsChanged(key.clone()).into()
    }

    /// The `reconnected` devices came back online. The agent writes their
    /// saved haptic settings again, and answers a read only once that write
    /// has finished, so each is read again for what it holds now.
    pub(super) fn haptics_reconnected(&mut self, reconnected: &[DeviceKey]) {
        for key in reconnected {
            self.pointer.reads.refresh_haptics(key);
        }
    }

    pub(super) fn load_current_haptics(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.haptic_feedback)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_haptics(key, route, self.ipc_sender(), cx);
    }
}

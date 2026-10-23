//! Press force of a force-sensing button (HID++ `0x19c0`), such as the
//! MX Master 4's Haptic Sense Panel: the device's reading and the persisted
//! preference.
//!
//! The *reading* is the swr-backed device query the device-read service
//! owns: the force the button clicks at now, the range the device accepts,
//! and whether software may change it. The slider shows the reading. The
//! *setting* is `config.toml`'s per-device `force_sensing`, which the agent
//! writes again when the device reconnects.
//!
//! A release of the slider writes the device through the agent. Only the
//! answer to the newest write counts: when the device reads back the force
//! that write asked for, it is saved, so a refused or superseded write never
//! becomes the setting. A refusal sends the query back to the device and
//! marks it refused until the next write lands or the device reconnects.

use openlogi_core::hid::{ForceSensingInfo, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::ForceSensingLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device has a force-sensing button whose force
    /// software may change, as its probe measured.
    #[must_use]
    pub fn current_force_sensing_supported(&self) -> bool {
        self.current_record().is_some_and(senses_force)
    }

    /// What is known of the active device's press-force reading.
    #[must_use]
    pub fn current_force_sensing_load(&self) -> ForceSensingLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.force_sensing_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The active device's press-force reading when software may change the
    /// force: its probe found a force-sensing button and its last reading is
    /// in and says the force is changeable. `None` while the reading is
    /// unknown, loading, failed or unsupported, or says the force is fixed.
    #[must_use]
    pub fn current_force_sensing_reading(&self) -> Option<ForceSensingInfo> {
        if !self.current_force_sensing_supported() {
            return None;
        }
        match self.current_force_sensing_load() {
            ForceSensingLoad::Ready(info) if info.changeable => Some(*info),
            ForceSensingLoad::Ready(_)
            | ForceSensingLoad::Unknown
            | ForceSensingLoad::Loading
            | ForceSensingLoad::Failed(_)
            | ForceSensingLoad::Unsupported(_) => None,
        }
    }

    /// Whether the active device refused its last press-force write. The
    /// slider then shows the force the device read back, not the one asked
    /// for.
    #[must_use]
    pub fn current_force_sensing_refused(&self) -> bool {
        self.current_record().is_some_and(|record| {
            self.pointer
                .refused_force_sensing
                .contains(&record.device_key())
        })
    }

    /// Write `force`, clamped into the range the device reported, to the
    /// active device through the agent. The slider shows it at once; the
    /// device's read-back arrives as [`Self::apply_force_sensing_written`],
    /// which saves it if this is still the newest write and the device took
    /// the force. No-op unless the device has a route and its last reading
    /// says the force may change.
    pub fn commit_force_sensing(&mut self, force: u16) -> StateEvents {
        let events = self.for_current_device(StateEvent::ForceSensingChanged);
        let Some(info) = self.current_force_sensing_reading() else {
            debug!(
                force,
                "no changeable press-force reading — ignoring the release"
            );
            return events;
        };
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return events;
        };
        let force = info.clamp(force);
        self.pointer.refused_force_sensing.remove(&key);
        self.pointer.reads.set_force_sensing_ready(
            &key,
            ForceSensingInfo {
                current: force,
                ..info
            },
        );
        let write = self.pointer.next_force_sensing_write;
        self.pointer.next_force_sensing_write = write.saturating_add(1);
        self.pointer
            .pending_force_sensing_writes
            .insert(key.clone(), write);
        self.send_ipc(crate::services::ipc::SetForceSensing {
            route,
            force,
            key,
            write,
        });
        events
    }

    /// The force a reset puts the active device back to: the out-of-box
    /// force its reading reports. `None` while there is no changeable
    /// reading, or when the force already sits there.
    #[must_use]
    pub fn current_force_sensing_reset(&self) -> Option<u16> {
        self.current_force_sensing_reading()
            .filter(|info| info.current != info.default)
            .map(|info| info.default)
    }

    /// Put the active device's press force back to its out-of-box force.
    /// The force-sensing feature has no reset command, so this is a
    /// [`Self::commit_force_sensing`] of the reported default, numbered,
    /// read back and saved like a slider release. No-op while
    /// [`Self::current_force_sensing_reset`] is `None`.
    pub fn reset_force_sensing(&mut self) -> StateEvents {
        match self.current_force_sensing_reset() {
            Some(force) => self.commit_force_sensing(force),
            None => StateEvents::none(),
        }
    }

    /// The device's answer to the [`Self::commit_force_sensing`] write
    /// numbered `write`, which asked for `force`.
    ///
    /// Only the newest write's answer counts; an older one changes nothing,
    /// so the slider stays on the newer release. The agent answers a write
    /// that a newer one superseded before its turn with a plain read, so an
    /// `Ok` is saved only when the device reads back `force`; any other
    /// reading is shown and nothing is saved. When the device refused or
    /// could not be reached, it is asked again so the optimistic value does
    /// not stand in for its own, and marked refused for
    /// [`Self::current_force_sensing_refused`].
    pub fn apply_force_sensing_written(
        &mut self,
        key: &DeviceKey,
        write: u64,
        force: u16,
        result: Result<ForceSensingInfo, WriteError>,
    ) -> StateEvents {
        if self.pointer.pending_force_sensing_writes.get(key) != Some(&write) {
            debug!(key = %key, write, force, "a newer press-force write is pending — ignoring this answer");
            return StateEvents::none();
        }
        self.pointer.pending_force_sensing_writes.remove(key);
        match result {
            Ok(info) => {
                self.pointer.refused_force_sensing.remove(key);
                self.pointer.reads.set_force_sensing_ready(key, info);
                if info.current == force {
                    self.save_force_sensing(key, force);
                } else {
                    debug!(key = %key, requested = force, actual = info.current, "the device runs another press force — not saving the request");
                }
            }
            Err(error) => {
                warn!(%error, key = %key, "press-force write did not land — re-reading the device");
                self.pointer.refused_force_sensing.insert(key.clone());
                self.pointer.reads.refresh_force_sensing(key);
            }
        }
        StateEvent::ForceSensingChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted press-force read — the "click to retry"
    /// affordance on a [`ForceSensingLoad::Failed`] device.
    pub(crate) fn retry_force_sensing_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_force_sensing(key);
        StateEvent::ForceSensingChanged(key.clone()).into()
    }

    /// `key`'s device came back online. The agent writes the saved force
    /// again when a device connects, so a refused write no longer stands,
    /// and the device is read again so the slider follows it.
    pub(super) fn force_sensing_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.refused_force_sensing.remove(key);
        self.pointer.reads.refresh_force_sensing(key);
    }

    pub(super) fn load_current_force_sensing(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| senses_force(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the device's press force");
        self.pointer
            .reads
            .ensure_force_sensing(key, route, self.ipc_sender(), cx);
    }

    /// Save `force` as `key`'s press force when the device persists
    /// settings and the force differs from the saved one.
    fn save_force_sensing(&mut self, key: &DeviceKey, force: u16) {
        let Some(persistent_key) = self
            .devices
            .records
            .iter()
            .find(|record| record.config_key == key.as_str())
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        if self.config.force_sensing(&persistent_key) == Some(force) {
            return;
        }
        self.config
            .edit(|config| config.set_force_sensing(&persistent_key, force));
        self.persist_and_reload("press force");
    }
}

fn senses_force(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.force_sensing)
}

//! Brightness of a keypad's display keys (HID++ `0x8040`), such as the MX
//! Creative Keypad's LCD keys: the keypad's reading and the persisted
//! preference.
//!
//! The *reading* is the swr-backed device query the device-read service
//! owns: the brightness the keys show now, as a percentage of the range the
//! keypad reports. The slider shows it. The *setting* is `config.toml`'s
//! per-device `key_display_brightness`, which the agent writes again when
//! the keypad reconnects.
//!
//! A release of the slider writes the keypad through the agent, as the
//! press force does ([`super::force_sensing`]). Only the answer to the
//! newest write counts: when the keypad reads back the brightness that
//! write asked for, it is saved, so a refused or superseded write never
//! becomes the setting. A refusal sends the query back to the keypad and
//! marks it refused until the next write lands or the keypad reconnects.

use std::collections::HashMap;

use openlogi_core::config::KeyDisplayBrightness;
use openlogi_core::hid::WriteError;
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::KeyBrightnessLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the selected device measured a settable display-key
    /// brightness (HID++ `0x8040`). Being a keypad is not enough: the
    /// control shows only once the device has reported the feature.
    #[must_use]
    pub fn current_key_brightness_supported(&self) -> bool {
        self.current_record().is_some_and(has_key_brightness)
    }

    /// What is known of the active keypad's brightness reading.
    #[must_use]
    pub fn current_key_brightness_load(&self) -> KeyBrightnessLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.key_brightness_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The brightness the slider shows for the active keypad: its reading
    /// once in, else the saved level, else `None` while neither is known.
    #[must_use]
    pub fn current_key_brightness(&self) -> Option<KeyDisplayBrightness> {
        match self.current_key_brightness_load() {
            KeyBrightnessLoad::Ready(brightness) => Some(*brightness),
            KeyBrightnessLoad::Unknown
            | KeyBrightnessLoad::Loading
            | KeyBrightnessLoad::Failed(_)
            | KeyBrightnessLoad::Unsupported(_) => self
                .current_record()
                .and_then(|record| self.device_key_display_brightness(&record.config_key)),
        }
    }

    /// Whether the active keypad refused its last brightness write, or the
    /// agent could not deliver it. The slider then shows the brightness the
    /// keypad read back, not the one asked for.
    #[must_use]
    pub fn current_key_brightness_refused(&self) -> bool {
        self.current_record().is_some_and(|record| {
            self.pointer
                .key_brightness_writes
                .is_refused(&record.device_key())
        })
    }

    /// The number of the brightness write to `key` still waiting for its
    /// answer, if any.
    #[cfg(test)]
    pub(crate) fn pending_key_brightness_write(&self, key: &DeviceKey) -> Option<u64> {
        self.pointer.key_brightness_writes.pending(key)
    }

    /// Write `brightness` to the active keypad through the agent. The slider
    /// shows it at once; the keypad's read-back arrives as
    /// [`Self::apply_key_brightness_written`], which saves it if this is
    /// still the newest write and the keypad took it. No-op unless the
    /// keypad measured `0x8040` and has a route.
    pub fn commit_key_brightness(&mut self, brightness: KeyDisplayBrightness) -> StateEvents {
        let events = self.for_current_device(StateEvent::KeyBrightnessChanged);
        let Some((key, route)) = self
            .current_record()
            .filter(|record| has_key_brightness(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            debug!(%brightness, "no keypad with display-key brightness — ignoring the release");
            return events;
        };
        self.pointer
            .reads
            .set_key_brightness_ready(&key, brightness);
        let write = self.pointer.key_brightness_writes.begin(&key);
        self.send_ipc(crate::services::ipc::SetKeyBrightness {
            route,
            brightness,
            key,
            write,
        });
        events
    }

    /// The keypad's answer to the [`Self::commit_key_brightness`] write
    /// numbered `write`, which asked for `brightness`.
    ///
    /// Only the newest write's answer counts; an older one changes nothing,
    /// so the slider stays on the newer release. The agent answers a write
    /// that a newer one superseded before its turn with a plain read, so an
    /// `Ok` is saved only when the keypad reads back `brightness`; any other
    /// reading is shown and nothing is saved. When the keypad refused or
    /// could not be reached, it is asked again so the optimistic value does
    /// not stand in for its own, and marked refused for
    /// [`Self::current_key_brightness_refused`].
    pub fn apply_key_brightness_written(
        &mut self,
        key: &DeviceKey,
        write: u64,
        brightness: KeyDisplayBrightness,
        result: Result<KeyDisplayBrightness, WriteError>,
    ) -> StateEvents {
        let writes = &mut self.pointer.key_brightness_writes;
        let newest = match &result {
            Ok(_) => writes.landed(key, write),
            Err(_) => writes.refused(key, write),
        };
        if !newest {
            debug!(key = %key, write, %brightness, "a newer brightness write is pending — ignoring this answer");
            return StateEvents::none();
        }
        match result {
            Ok(read) => {
                self.pointer.reads.set_key_brightness_ready(key, read);
                if read == brightness {
                    self.save_key_brightness(key, brightness);
                } else {
                    debug!(key = %key, requested = %brightness, actual = %read, "the keypad shows another brightness — not saving the request");
                }
            }
            Err(error) => {
                warn!(%error, key = %key, "display-key brightness write did not land — re-reading the keypad");
                self.pointer.reads.refresh_key_brightness(key);
            }
        }
        StateEvent::KeyBrightnessChanged(key.clone()).into()
    }

    /// `key`'s keypad came back online. The agent writes the saved
    /// brightness again when a keypad connects, so a refused write no longer
    /// stands, and the keypad is read again so the slider follows it.
    pub(super) fn key_brightness_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.key_brightness_writes.reconnected(key);
        self.pointer.reads.refresh_key_brightness(key);
    }

    pub(super) fn load_current_key_brightness(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| has_key_brightness(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the keypad's display-key brightness");
        self.pointer
            .reads
            .ensure_key_brightness(key, route, self.ipc_sender(), cx);
    }

    /// Save `brightness` as `key`'s display-key brightness when the device
    /// persists settings and the level differs from the saved one.
    fn save_key_brightness(&mut self, key: &DeviceKey, brightness: KeyDisplayBrightness) {
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
        if self.config.key_display_brightness(&persistent_key) == Some(brightness) {
            return;
        }
        self.config.edit(|config| {
            config.set_key_display_brightness(&persistent_key, Some(brightness));
        });
        self.persist_and_reload("key display brightness");
    }
}

/// Where each keypad's display-key brightness writes stand: the one
/// transition owner for "which write's answer counts" and "did the newest
/// write land". Its methods are the only writers, so a keypad is never both
/// pending and refused, and a refusal never outlives the next write.
#[derive(Debug, Default)]
pub(super) struct KeyBrightnessWrites {
    /// Keypads not listed are idle: no write pending, the last one landed.
    by_device: HashMap<DeviceKey, WriteStage>,
    next: u64,
}

/// One keypad's brightness writes, unless idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStage {
    /// The newest write sent, numbered, waiting for its answer. Only that
    /// write's answer may change the shown brightness or save it.
    Pending(u64),
    /// The newest write did not land: the keypad refused it, or the agent
    /// could not deliver it.
    Refused,
}

impl KeyBrightnessWrites {
    /// Number a new write to `key`'s keypad. It supersedes any earlier one
    /// and clears a refusal.
    pub(super) fn begin(&mut self, key: &DeviceKey) -> u64 {
        let write = self.next;
        self.next = write.saturating_add(1);
        self.by_device
            .insert(key.clone(), WriteStage::Pending(write));
        write
    }

    /// The write numbered `write` landed. Answers whether it was the newest
    /// write to `key`'s keypad, which only then becomes idle.
    pub(super) fn landed(&mut self, key: &DeviceKey, write: u64) -> bool {
        let newest = self.is_pending(key, write);
        if newest {
            self.by_device.remove(key);
        }
        newest
    }

    /// The write numbered `write` did not land. Answers whether it was the
    /// newest write to `key`'s keypad, which only then is marked refused.
    pub(super) fn refused(&mut self, key: &DeviceKey, write: u64) -> bool {
        let newest = self.is_pending(key, write);
        if newest {
            self.by_device.insert(key.clone(), WriteStage::Refused);
        }
        newest
    }

    /// `key`'s keypad reconnected and the agent writes its saved level
    /// again, so a refusal no longer stands. A pending write stays pending.
    pub(super) fn reconnected(&mut self, key: &DeviceKey) {
        if self.is_refused(key) {
            self.by_device.remove(key);
        }
    }

    /// Whether `key`'s keypad did not take the newest write.
    pub(super) fn is_refused(&self, key: &DeviceKey) -> bool {
        self.by_device.get(key) == Some(&WriteStage::Refused)
    }

    /// The number of the write to `key`'s keypad still waiting for its
    /// answer, if any.
    #[cfg(test)]
    pub(super) fn pending(&self, key: &DeviceKey) -> Option<u64> {
        match self.by_device.get(key)? {
            WriteStage::Pending(write) => Some(*write),
            WriteStage::Refused => None,
        }
    }

    fn is_pending(&self, key: &DeviceKey, write: u64) -> bool {
        self.by_device.get(key) == Some(&WriteStage::Pending(write))
    }
}

fn has_key_brightness(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.key_brightness)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every transition from every stage: only the newest write's answer
    /// moves a keypad, a refusal never survives the next write, and a
    /// reconnect clears a refusal but not a pending write.
    #[test]
    fn writes_move_only_on_the_newest_answer() {
        let pad = DeviceKey::from("pad");
        let mut writes = KeyBrightnessWrites::default();
        assert_eq!(
            (writes.pending(&pad), writes.is_refused(&pad)),
            (None, false)
        );

        let older = writes.begin(&pad);
        let newer = writes.begin(&pad);
        assert_eq!(writes.pending(&pad), Some(newer));
        assert!(
            !writes.landed(&pad, older),
            "a superseded answer is not the newest"
        );
        assert!(!writes.refused(&pad, older), "nor is its failure");
        assert_eq!(writes.pending(&pad), Some(newer));

        writes.reconnected(&pad);
        assert_eq!(
            writes.pending(&pad),
            Some(newer),
            "a reconnect keeps a pending write"
        );

        assert!(writes.refused(&pad, newer));
        assert!(writes.is_refused(&pad));
        assert_eq!(writes.pending(&pad), None);
        assert!(
            !writes.landed(&pad, newer),
            "an answered write is not answered twice"
        );

        let next = writes.begin(&pad);
        assert!(!writes.is_refused(&pad), "a new write clears the refusal");
        assert!(writes.landed(&pad, next));
        assert_eq!(
            (writes.pending(&pad), writes.is_refused(&pad)),
            (None, false)
        );

        let failed = writes.begin(&pad);
        assert!(writes.refused(&pad, failed));
        writes.reconnected(&pad);
        assert!(!writes.is_refused(&pad), "a reconnect clears a refusal");
        assert!(
            !writes.is_refused(&DeviceKey::from("other")),
            "keypads are independent"
        );
    }
}

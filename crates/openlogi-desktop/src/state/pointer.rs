//! Pointer-device query and active-editor state.

use std::collections::{HashMap, HashSet};

use openlogi_core::hid::Dpi;
use swr_core::{Runtime, SwrClient};

use crate::services::device_reads::DeviceReads;

use super::device_key::DeviceKey;
use super::{AppState, DEFAULT_DPI};

pub(super) struct PointerState {
    pub(super) dpi: Dpi,
    pub(super) reads: DeviceReads,
    pub(super) next_smartshift_write_id: u64,
    /// The DPI writes sent and not answered yet.
    pub(super) dpi_writes: DpiWrites,
    /// The mice whose last pointer-speed write the mouse refused or the agent
    /// could not deliver.
    pub(super) refused_pointer_speeds: HashSet<DeviceKey>,
    /// The devices whose last press-force write the device refused or the
    /// agent could not deliver.
    pub(super) refused_force_sensing: HashSet<DeviceKey>,
    /// The newest press-force write sent to each device whose answer has not
    /// arrived yet. Only that write's answer may change the shown force or
    /// save it.
    pub(super) pending_force_sensing_writes: HashMap<DeviceKey, u64>,
    pub(super) next_force_sensing_write: u64,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_DPI,
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
            dpi_writes: DpiWrites::default(),
            refused_pointer_speeds: HashSet::new(),
            refused_force_sensing: HashSet::new(),
            pending_force_sensing_writes: HashMap::new(),
            next_force_sensing_write: 0,
        }
    }
}

/// What sent a DPI write, which decides how the device taking it is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DpiWrite {
    /// The DPI panel: saved as the device's DPI.
    Ordinary,
    /// A settings transfer, which replaces the target's per-link overrides
    /// of what it copies: saved on every link.
    Copied,
}

/// An answered DPI write, as [`DpiWrites::finish`] hands it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AnsweredDpiWrite {
    /// What sent it.
    pub(super) kind: DpiWrite,
    /// Whether it is the newest write sent to its device. Only that write's
    /// answer may change the shown DPI.
    pub(super) newest: bool,
}

/// The DPI writes sent and not answered yet: each write's device and kind,
/// and the newest write per device. [`Self::start`] and [`Self::finish`] are
/// the only ways in and out, so an answer always finds the write it answers.
#[derive(Debug, Default)]
pub(super) struct DpiWrites {
    next: u64,
    in_flight: HashMap<u64, (DeviceKey, DpiWrite)>,
    newest: HashMap<DeviceKey, u64>,
}

impl DpiWrites {
    /// Record a `kind` write to `key`'s device as its newest, and return the
    /// number its answer carries.
    pub(super) fn start(&mut self, key: DeviceKey, kind: DpiWrite) -> u64 {
        let write = self.next;
        self.next = write.saturating_add(1);
        self.newest.insert(key.clone(), write);
        self.in_flight.insert(write, (key, kind));
        write
    }

    /// Whether the newest write to `key`'s device is still waiting for its
    /// answer, which then decides the DPI shown for it.
    pub(super) fn newest_awaits_answer(&self, key: &DeviceKey) -> bool {
        self.newest.contains_key(key)
    }

    /// Take the write `write` that `key`'s device answered. `None` for a
    /// write that was never started, was already answered, or went to
    /// another device.
    pub(super) fn finish(&mut self, key: &DeviceKey, write: u64) -> Option<AnsweredDpiWrite> {
        if self
            .in_flight
            .get(&write)
            .is_none_or(|(sent_to, _)| sent_to != key)
        {
            return None;
        }
        let (_, kind) = self.in_flight.remove(&write)?;
        let newest = self.newest.get(key) == Some(&write);
        if newest {
            self.newest.remove(key);
        }
        Some(AnsweredDpiWrite { kind, newest })
    }
}

impl AppState {
    pub(crate) fn connect_device_reads(
        &mut self,
        client: SwrClient,
        runtime: std::sync::Arc<dyn Runtime>,
    ) {
        self.pointer.reads.connect(client.clone(), runtime.clone());
        self.disable_keys_reads.connect(client, runtime);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(name: &str) -> DeviceKey {
        DeviceKey::from(name)
    }

    #[test]
    fn an_answer_finishes_the_write_it_answers_once() {
        let mut writes = DpiWrites::default();
        let copied = writes.start(mouse("a"), DpiWrite::Copied);

        assert_eq!(
            writes.finish(&mouse("a"), copied),
            Some(AnsweredDpiWrite {
                kind: DpiWrite::Copied,
                newest: true,
            })
        );
        assert_eq!(
            writes.finish(&mouse("a"), copied),
            None,
            "a write is answered once"
        );
    }

    #[test]
    fn an_answer_no_write_matches_is_refused() {
        let mut writes = DpiWrites::default();
        let write = writes.start(mouse("a"), DpiWrite::Ordinary);

        assert_eq!(writes.finish(&mouse("a"), write + 1), None, "never sent");
        assert_eq!(writes.finish(&mouse("b"), write), None, "another device's");
        assert_eq!(
            writes
                .finish(&mouse("a"), write)
                .map(|answered| answered.kind),
            Some(DpiWrite::Ordinary),
            "a refused answer leaves the write in flight"
        );
    }

    #[test]
    fn only_the_newest_write_to_a_device_is_newest() {
        let mut writes = DpiWrites::default();
        let first = writes.start(mouse("a"), DpiWrite::Ordinary);
        let other = writes.start(mouse("b"), DpiWrite::Ordinary);
        let second = writes.start(mouse("a"), DpiWrite::Copied);

        let newest = |answered: Option<AnsweredDpiWrite>| answered.map(|answered| answered.newest);
        assert_eq!(newest(writes.finish(&mouse("a"), first)), Some(false));
        assert!(
            writes.newest_awaits_answer(&mouse("a")),
            "the newer one is out"
        );
        assert_eq!(newest(writes.finish(&mouse("b"), other)), Some(true));
        assert_eq!(newest(writes.finish(&mouse("a"), second)), Some(true));
        assert!(!writes.newest_awaits_answer(&mouse("a")));
    }
}

//! The immutable probe cache's persistable form, and the port that keeps it.
//!
//! A device's expensive probe result (model info, capabilities, feature
//! indexes) is immutable, so it only ever needs to be read once per device —
//! but the in-memory cache dies with the process, forcing every agent restart
//! to re-interview every device. Persisting the cache means a device that was
//! fully probed once keeps its identity across restarts, even on transports
//! where a fresh walk is slow or failing (see `BOLT_SLOT_PROBE`).
//!
//! Only Bolt identities are persisted, because only they are keyed on the
//! device's *own* identity (the pairing-register unit id), which no re-pairing
//! can silently reassign. A `CacheKey::UnifyingSlot` is `receiver + slot`: a
//! different device paired into that slot while the agent is down would
//! inherit the previous occupant's probe on warm start. A `CacheKey::Direct`
//! is an OS-runtime node id with no cross-boot stability. Loaded entries get
//! `probed_tick = 0`, so the regular `cache::REFRESH_TICKS`
//! self-healing pass re-walks them on schedule; until (and unless) that walk
//! succeeds, the persisted data serves exactly like an in-memory cache hit.
//!
//! *Where* a snapshot is kept is the host's business, not this module's: the
//! enumerator writes through a [`ProbeCacheStore`], and every native build
//! supplies [`FileProbeCacheStore`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::cache::{CacheKey, Cached};
use super::features::{BatteryProbe, ProbedFeatures};

mod file;

pub use file::FileProbeCacheStore;

/// Bumped when the persisted shape changes; a mismatched snapshot is discarded
/// (the cache is a warm-start optimization, not data anyone must keep).
/// v2 dropped the `UnifyingSlot` key (slot-keyed, so not re-pair-safe).
const SCHEMA_VERSION: u32 = 2;

/// A probe-cache store could not keep a snapshot.
///
/// Carries only a message: nothing branches on why a best-effort write failed,
/// and the reasons differ per store (a filesystem error, a browser storage
/// quota).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ProbeCacheError(String);

/// Where an [`Enumerator`](super::Enumerator)'s probe cache lives between runs.
///
/// A port, like [`HidBackend`](crate::backend::HidBackend): the enumerator
/// knows what is worth keeping and when it changed, and nothing about where it
/// goes. Native builds use [`FileProbeCacheStore`].
pub trait ProbeCacheStore: Send + Sync {
    /// The last snapshot saved here, or an empty one.
    ///
    /// Never fails: an absent, torn or foreign-schema store is a cold start,
    /// which the enumerator handles by re-probing — not an error worth
    /// propagating into device discovery.
    fn load(&self) -> ProbeCacheSnapshot;

    /// Persist `snapshot`.
    ///
    /// An `Err` is logged by the caller and the snapshot retried on the next
    /// tick that dirties the cache, so a store may fail freely.
    fn save(&self, snapshot: &ProbeCacheSnapshot) -> Result<(), ProbeCacheError>;
}

/// The persistable subset of the probe cache, in the shape a store keeps.
#[derive(Serialize, Deserialize)]
pub struct ProbeCacheSnapshot {
    version: u32,
    entries: Vec<PersistedEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: PersistedKey,
    probe: ProbedFeatures,
    battery: Option<BatteryProbe>,
}

/// The persistable subset of [`CacheKey`] — Bolt only (see the module docs).
#[derive(Clone, Copy, Serialize, Deserialize)]
enum PersistedKey {
    Bolt { unit_id: [u8; 4] },
}

fn persistable(key: &CacheKey) -> Option<PersistedKey> {
    match key {
        CacheKey::Bolt { unit_id } => Some(PersistedKey::Bolt { unit_id: *unit_id }),
        CacheKey::UnifyingSlot { .. } | CacheKey::Direct(_) => None,
    }
}

/// Whether a cache change under `key` affects the persisted snapshot at all —
/// gates `cache_dirty` so churn on never-persisted keys (e.g. a direct-only
/// system's full refresh) doesn't rewrite an unchanged snapshot every pass.
pub(super) fn is_persistable(key: &CacheKey) -> bool {
    persistable(key).is_some()
}

fn runtime_key(key: PersistedKey) -> CacheKey {
    match key {
        PersistedKey::Bolt { unit_id } => CacheKey::Bolt { unit_id },
    }
}

impl ProbeCacheSnapshot {
    /// A snapshot carrying nothing — what a store with no readable content
    /// returns, and a cold start for the enumerator.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    /// Everything in `cache` worth keeping across restarts.
    pub(super) fn of(cache: &HashMap<CacheKey, Cached>) -> Self {
        let entries = cache
            .iter()
            .filter_map(|(key, cached)| {
                persistable(key).map(|key| {
                    // The battery *reading* is volatile and re-read live on
                    // every cache hit — persisting it would resurrect a stale
                    // value after a restart. The battery *feature index*
                    // (`PersistedEntry::battery`) is immutable and kept.
                    let mut probe = cached.probe.clone();
                    probe.battery = None;
                    PersistedEntry {
                        key,
                        probe,
                        battery: cached.battery,
                    }
                })
            })
            .collect();
        Self {
            version: SCHEMA_VERSION,
            entries,
        }
    }

    /// Fold this snapshot back into runtime cache entries.
    ///
    /// A snapshot written by another schema version yields nothing: the shape
    /// it describes is not the one this build reads, and re-probing is always
    /// correct.
    pub(super) fn into_entries(self) -> HashMap<CacheKey, Cached> {
        if self.version != SCHEMA_VERSION {
            tracing::debug!(
                version = self.version,
                "probe cache from another schema — starting cold"
            );
            return HashMap::new();
        }
        self.entries
            .into_iter()
            .map(|entry| {
                (
                    runtime_key(entry.key),
                    Cached {
                        probe: entry.probe,
                        battery: entry.battery,
                        // Restart the refresh clock: the entry serves
                        // immediately as a cache hit, and the periodic
                        // self-healing re-walk decides when it is due for a
                        // fresh read.
                        probed_tick: 0,
                    },
                )
            })
            .collect()
    }
}

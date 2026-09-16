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
//! is an OS-runtime node id with no cross-boot stability. Loaded entries
//! restart the elapsed refresh window, so the regular self-healing pass
//! re-walks them on schedule; until (and unless) that walk succeeds, the
//! persisted data serves exactly like an in-memory cache hit.
//!
//! *Where* a snapshot is kept is the host's business, not this module's: the
//! enumerator writes through a [`ProbeCacheStore`]; `openlogi-hid` supplies
//! the file-backed one every native build uses.

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::cache::{CacheKey, Cached};
use super::events::EventFeatureIndices;
use super::features::{BatteryProbe, ProbedFeatures};

/// Bumped when the persisted shape changes; a mismatched snapshot is discarded
/// (the cache is a warm-start optimization, not data anyone must keep).
/// v2 dropped the `UnifyingSlot` key (slot-keyed, so not re-pair-safe).
/// v3 adds event-capable feature indexes discovered by the immutable walk.
const SCHEMA_VERSION: u32 = 3;

impl ProbeCacheError {
    /// Report why a store could not keep a snapshot.
    #[must_use]
    pub fn new(reason: impl std::fmt::Display) -> Self {
        Self(reason.to_string())
    }
}

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
/// goes. `openlogi-hid` supplies the file-backed one native builds use.
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
    /// pass that dirties the cache, so a store may fail freely.
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
    events: EventFeatureIndices,
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

    /// Whether this snapshot carries nothing — a store may skip writing one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
                        events: cached.events,
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
                        events: entry.events,
                        // Restart the refresh clock: the entry serves
                        // immediately as a cache hit, and the periodic
                        // self-healing re-walk decides when it is due for a
                        // fresh read.
                        probed_at: Instant::now(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceModelInfo, DeviceTransports,
    };

    use super::super::cache::{CacheKey, Cached};
    use super::super::events::EventFeatureIndices;
    use super::super::features::{BatteryProbe, ProbedFeatures};
    use super::{ProbeCacheSnapshot, SCHEMA_VERSION};

    /// A device fully probed once keeps its identity across restarts — that is
    /// the whole point of the snapshot — but only the parts that are actually
    /// immutable, and only for keys a re-pair cannot silently reassign.
    #[test]
    fn a_snapshot_keeps_bolt_identity_and_drops_the_volatile_reading() {
        let model = DeviceModelInfo {
            entity_count: 1,
            serial_number: Some("TESTSERIAL01".into()),
            unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            transports: DeviceTransports::default(),
            model_ids: [0xb042, 0, 0],
            extended_model_id: 0,
        };
        let mut cache = HashMap::new();
        cache.insert(
            CacheKey::Bolt {
                unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            },
            Cached {
                probe: ProbedFeatures {
                    model_info: Some(model.clone()),
                    // A live reading at snapshot time.
                    battery: Some(BatteryInfo {
                        percentage: 55,
                        level: BatteryLevel::Good,
                        status: BatteryStatus::Discharging,
                    }),
                    ..Default::default()
                },
                battery: Some(BatteryProbe::Unified(9)),
                events: EventFeatureIndices {
                    wireless_status: Some(7),
                    unified_battery: Some(9),
                },
                probed_at: Instant::now(),
            },
        );
        cache.insert(
            CacheKey::UnifyingSlot {
                receiver_uid: "DA2699E1".into(),
                slot: 2,
            },
            Cached {
                probe: ProbedFeatures::default(),
                battery: None,
                events: EventFeatureIndices::default(),
                probed_at: Instant::now(),
            },
        );

        let snapshot = ProbeCacheSnapshot::of(&cache);
        let restored_after = Instant::now();
        let restored = snapshot.into_entries();

        let bolt = restored
            .get(&CacheKey::Bolt {
                unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            })
            .expect("a Bolt entry survives");
        assert_eq!(bolt.probe.model_info.as_ref(), Some(&model));
        assert_eq!(
            bolt.battery,
            Some(BatteryProbe::Unified(9)),
            "the battery *feature index* is immutable and kept"
        );
        assert_eq!(
            bolt.events,
            EventFeatureIndices {
                wireless_status: Some(7),
                unified_battery: Some(9),
            },
            "event feature indexes are immutable and kept"
        );
        assert!(
            bolt.probe.battery.is_none(),
            "the battery *reading* is volatile — restoring it would resurrect a stale value"
        );
        assert!(
            bolt.probed_at >= restored_after,
            "a restored entry restarts the refresh clock"
        );
        assert!(
            !restored.contains_key(&CacheKey::UnifyingSlot {
                receiver_uid: "DA2699E1".into(),
                slot: 2,
            }),
            "unifying entries are slot-keyed, so a re-pair while the agent is \
             down could hand them to a different device — never persisted"
        );
    }

    /// A snapshot written by another schema describes a shape this build does
    /// not read. Re-probing is always correct; guessing is not.
    #[test]
    fn a_foreign_schema_yields_a_cold_start() {
        let mut snapshot = ProbeCacheSnapshot::of(&HashMap::new());
        snapshot.version = SCHEMA_VERSION + 1;

        assert!(snapshot.into_entries().is_empty());
    }
}

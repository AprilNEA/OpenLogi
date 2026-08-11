//! Disk persistence for the immutable probe cache.
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
//! can silently reassign. A [`CacheKey::UnifyingSlot`] is `receiver + slot`: a
//! different device paired into that slot while the agent is down would
//! inherit the previous occupant's probe on warm start. A [`CacheKey::Direct`]
//! is an OS-runtime node id with no cross-boot stability. Loaded entries get
//! `probed_tick = 0`, so the regular [`super::cache::REFRESH_TICKS`]
//! self-healing pass re-walks them on schedule; until (and unless) that walk
//! succeeds, the persisted data serves exactly like an in-memory cache hit.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::cache::{CacheKey, Cached};
use super::features::{BatteryProbe, ProbedFeatures};

/// Bumped when the on-disk shape changes; a mismatched file is discarded
/// (the cache is a warm-start optimization, not data anyone must keep).
/// v2 dropped the `UnifyingSlot` key (slot-keyed, so not re-pair-safe).
const SCHEMA_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct PersistedCache {
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

/// Whether a cache change under `key` affects the persisted file at all —
/// gates `cache_dirty` so churn on never-persisted keys (e.g. a direct-only
/// system's full refresh) doesn't rewrite an unchanged file every pass.
pub(super) fn is_persistable(key: &CacheKey) -> bool {
    persistable(key).is_some()
}

fn runtime_key(key: PersistedKey) -> CacheKey {
    match key {
        PersistedKey::Bolt { unit_id } => CacheKey::Bolt { unit_id },
    }
}

/// Write the persistable subset of `cache` to `path`, atomically via a sibling
/// temp file so a crash mid-write can't leave a torn file. Errors are the
/// caller's to log — persistence is best-effort and must never fail
/// enumeration.
pub(super) fn save(path: &Path, cache: &HashMap<CacheKey, Cached>) -> io::Result<()> {
    let entries: Vec<PersistedEntry> = cache
        .iter()
        .filter_map(|(key, cached)| {
            persistable(key).map(|key| {
                // The battery *reading* is volatile and re-read live on every
                // cache hit — persisting it would resurrect a stale value
                // after a restart. The battery *feature index*
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
    let file = PersistedCache {
        version: SCHEMA_VERSION,
        entries,
    };
    let json = serde_json::to_vec(&file).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Load a previously saved cache. Any failure — missing file, unreadable
/// JSON, unknown schema version — yields an empty cache: the data is a
/// warm-start optimization, never a correctness requirement.
pub(super) fn load(path: &Path) -> HashMap<CacheKey, Cached> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<PersistedCache>(&bytes) else {
        tracing::warn!(?path, "probe cache unreadable — starting cold");
        return HashMap::new();
    };
    if file.version != SCHEMA_VERSION {
        tracing::debug!(
            version = file.version,
            "probe cache from another schema — starting cold"
        );
        return HashMap::new();
    }
    file.entries
        .into_iter()
        .map(|entry| {
            (
                runtime_key(entry.key),
                Cached {
                    probe: entry.probe,
                    battery: entry.battery,
                    // Restart the refresh clock: the entry serves immediately
                    // as a cache hit, and the periodic self-healing re-walk
                    // decides when it is due for a fresh read.
                    probed_tick: 0,
                },
            )
        })
        .collect()
}

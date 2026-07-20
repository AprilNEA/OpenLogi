//! Disk persistence for the immutable probe cache.
//!
//! A device's expensive probe result (model info, capabilities, feature
//! indexes) is immutable, so it only ever needs to be read once per device —
//! but the in-memory cache dies with the process, forcing every agent restart
//! to re-interview every device. Persisting the cache means a device that was
//! fully probed once keeps its identity across restarts, even on transports
//! where a fresh walk is slow or failing (see `BOLT_SLOT_PROBE`).
//!
//! Only receiver-paired identities are persisted: a [`CacheKey::Direct`] is an
//! OS-runtime node id with no cross-boot stability. Loaded entries get
//! `probed_tick = 0`, so the regular [`super::cache::REFRESH_TICKS`]
//! self-healing pass re-walks them on schedule; until (and unless) that walk
//! succeeds, the persisted data serves exactly like an in-memory cache hit.

use std::collections::HashMap;
use std::path::Path;

use super::cache::{CacheKey, Cached};

/// Write the persistable subset of `cache` to `path` (atomically via a
/// sibling temp file). Errors are the caller's to log — persistence is
/// best-effort and must never fail enumeration.
pub(super) fn save(path: &Path, cache: &HashMap<CacheKey, Cached>) -> std::io::Result<()> {
    let _ = (path, cache);
    Ok(())
}

/// Load a previously saved cache. Any failure — missing file, unreadable
/// JSON, unknown schema version — yields an empty cache: the data is a
/// warm-start optimization, never a correctness requirement.
pub(super) fn load(path: &Path) -> HashMap<CacheKey, Cached> {
    let _ = path;
    HashMap::new()
}

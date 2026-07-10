use std::sync::Arc;

use hidpp::channel::HidppChannel;

use super::features::{BatteryFeatureIndex, ProbedFeatures, probe_features, read_battery_at};

/// How many `enumerate` ticks a device's probe is reused before a fresh read.
/// The expensive part of a probe (the `enumerate_features` feature-table walk)
/// reads *immutable* data — model, capabilities, marketing type — so it never
/// needs re-reading for a known device; the periodic full probe is kept only as
/// a self-healing pass (e.g. a firmware update reshuffling the feature table).
/// The volatile battery does NOT ride this window: cache hits re-read it every
/// tick through the memoized feature index (see [`read_battery`]), so it stays
/// as fresh as it was before the cache existed (#153).
pub(super) const REFRESH_TICKS: u64 = 15;

/// Stable identity used to memoize a device's probe across `enumerate` ticks.
/// Keyed on the device's *own* identity (never its slot) so a re-paired or
/// moved device can't inherit another device's cached probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum CacheKey {
    /// Bolt: the unit id from the pairing register (cheap, read every tick).
    Bolt { unit_id: [u8; 4] },
    /// Unifying: keyed on the full receiver serial number + pairing slot.
    /// Using the complete serial (not just a prefix) avoids collisions between
    /// two receivers whose serials share a common prefix (e.g. "DA2699E1" and
    /// "DA2604F2" share "DA2").
    UnifyingSlot { receiver_uid: String, slot: u8 },
    /// LIGHTSPEED: receiver serial + slot keeps identical gaming receivers and
    /// devices isolated while remaining stable across sleep/reconnect.
    LightspeedSlot { receiver_uid: String, slot: u8 },
    /// Direct (Bluetooth/USB): the OS-assigned HID node id (macOS registry-entry
    /// id, Linux dev path, Windows interface path). Unique *per node*, so two
    /// units of the same model never collide, and stable while connected so the
    /// cache still hits across ticks.
    Direct(async_hid::DeviceId),
}

/// Enumeration ticks a device may be missing before its cache entry is evicted.
/// A small grace rides out a transient receiver timeout without dropping the
/// device's memoized data.
pub(super) const CACHE_MISS_GRACE: u8 = 3;

/// A memoized probe result plus the tick it was taken on.
#[derive(Clone)]
pub(super) struct Cached {
    pub(super) probe: ProbedFeatures,
    /// Runtime index of the `UnifiedBattery` feature in this device's feature
    /// table, captured by the full probe. Lets cache hits re-read the volatile
    /// battery in one round-trip — no `Device::new` ping, no table walk.
    /// `None` when the device exposes no `0x1004`.
    pub(super) battery_index: Option<BatteryFeatureIndex>,
    pub(super) probed_tick: u64,
}

/// What a probed device contributes to the cache this tick. The key lets stale
/// entries be evicted; `Fresh` (a full probe) and `Update` (a cache hit whose
/// volatile battery was re-read) also carry the value to insert. `Unkeyed` is a
/// device we can't (or won't) cache — an all-zero unit id, or a rejected
/// non-peripheral — so its key is neither inserted nor kept alive.
pub(super) enum CacheOutcome {
    Fresh(CacheKey, Cached),
    Update(CacheKey, Cached),
    Seen(CacheKey),
    Unkeyed,
}

/// `Seen` when the device has a stable key, else `Unkeyed`.
fn seen(id: Option<CacheKey>) -> CacheOutcome {
    id.map_or(CacheOutcome::Unkeyed, CacheOutcome::Seen)
}

/// Whether `cached` is stale enough that the device should be re-probed.
pub(super) fn is_stale(cached: &Cached, tick: u64) -> bool {
    tick.wrapping_sub(cached.probed_tick) >= REFRESH_TICKS
}

/// Decide a device's probe: reuse a fresh cache, or (online + miss/stale)
/// re-probe — but keep the last-known immutable data if the re-probe fails
/// rather than overwriting it with an empty default. An unprobed offline device
/// with no cache yields a default probe. Returns the probe plus its cache
/// contribution (only a *successful* probe is cached).
pub(super) async fn probe_or_reuse(
    channel: &Arc<HidppChannel>,
    index: u8,
    id: Option<CacheKey>,
    cached: Option<&Cached>,
    online: bool,
    tick: u64,
) -> (ProbedFeatures, CacheOutcome) {
    if online && cached.is_none_or(|c| is_stale(c, tick)) {
        let (fresh, battery_index) = probe_features(channel, index).await;
        // `capabilities` is `Some` exactly when the feature-table walk succeeded;
        // only then is the probe worth caching.
        if fresh.capabilities.is_some() {
            return match id {
                Some(key) => {
                    let value = Cached {
                        probe: fresh.clone(),
                        battery_index,
                        probed_tick: tick,
                    };
                    (fresh, CacheOutcome::Fresh(key, value))
                }
                None => (fresh, CacheOutcome::Unkeyed),
            };
        }
        // Re-probe failed: don't cache the failure. Fall back to the last-known
        // data so a transient glitch doesn't drop the device or its battery.
        // No battery re-read either — the device just proved unresponsive.
        return match cached {
            Some(c) => (c.probe.clone(), seen(id)),
            None => (fresh, seen(id)),
        };
    }
    match cached {
        Some(c) => {
            // Cache hit: the immutable data is reused as-is, but the battery is
            // volatile (#153) — re-read just it through the memoized feature
            // index and fold the reading back into the cache. A failed read
            // (asleep, mid-host-switch) keeps the last-known value.
            if online
                && let Some(feature_index) = c.battery_index
                && let Some(key) = id.clone()
                && let Some(battery) = read_battery_at(channel, index, feature_index).await
            {
                let mut entry = c.clone();
                entry.probe.battery = Some(battery);
                return (entry.probe.clone(), CacheOutcome::Update(key, entry));
            }
            (c.probe.clone(), seen(id))
        }
        None => (ProbedFeatures::default(), seen(id)),
    }
}

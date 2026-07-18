use std::collections::HashSet;

use super::Enumerator;
use super::cache::{CACHE_MISS_GRACE, CacheKey, Cached, REFRESH_TICKS, is_stale};
use crate::inventory::features::ProbedFeatures;

fn cache_entry(probed_tick: u64) -> Cached {
    Cached {
        probe: ProbedFeatures::default(),
        battery_index: None,
        probed_tick,
    }
}

#[test]
fn cache_entry_survives_grace_then_evicts() {
    let mut e = Enumerator::default();
    let key = CacheKey::Bolt {
        unit_id: [1, 2, 3, 4],
    };
    e.cache.insert(key.clone(), cache_entry(0));
    let nobody = HashSet::new();
    // Missing for the whole grace window: kept.
    for _ in 0..CACHE_MISS_GRACE {
        e.evict_unseen(&nobody);
        assert!(
            e.cache.contains_key(&key),
            "evicted inside the grace window"
        );
    }
    // One miss past the grace: evicted.
    e.evict_unseen(&nobody);
    assert!(
        !e.cache.contains_key(&key),
        "should evict past the grace window"
    );
}

#[test]
fn being_seen_resets_the_miss_counter() {
    let mut e = Enumerator::default();
    let key = CacheKey::Bolt { unit_id: [9; 4] };
    e.cache.insert(key.clone(), cache_entry(0));
    let nobody = HashSet::new();
    let seen: HashSet<CacheKey> = std::iter::once(key.clone()).collect();
    e.evict_unseen(&nobody); // miss 1
    e.evict_unseen(&seen); // seen → counter reset
    for _ in 0..CACHE_MISS_GRACE {
        e.evict_unseen(&nobody);
    }
    assert!(
        e.cache.contains_key(&key),
        "counter reset by a sighting, so still within grace"
    );
}

#[test]
fn cached_probe_is_reused_until_refresh_ticks() {
    let cached = Cached {
        probe: ProbedFeatures::default(),
        battery_index: None,
        probed_tick: 10,
    };
    assert!(!is_stale(&cached, 10), "same tick is fresh");
    assert!(
        !is_stale(&cached, 10 + REFRESH_TICKS - 1),
        "just under the window is still fresh"
    );
    assert!(
        is_stale(&cached, 10 + REFRESH_TICKS),
        "at the window the probe is refreshed"
    );
}

#[test]
fn lightspeed_cache_is_scoped_to_receiver_and_slot() {
    let first = CacheKey::LightspeedSlot {
        receiver_uid: "RX-A".into(),
        slot: 1,
    };
    let other_receiver = CacheKey::LightspeedSlot {
        receiver_uid: "RX-B".into(),
        slot: 1,
    };
    let other_slot = CacheKey::LightspeedSlot {
        receiver_uid: "RX-A".into(),
        slot: 2,
    };
    assert_ne!(first, other_receiver);
    assert_ne!(first, other_slot);
}

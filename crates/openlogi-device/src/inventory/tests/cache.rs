//! The probe cache: what it keys, when an entry ages out, and when a cached answer is reused.

use super::*;

#[test]
fn direct_codename_prefers_hidpp_marketing_name_over_generic_os_name() {
    assert_eq!(
        preferred_direct_codename(Some("Wireless Mouse MX Master 2S"), "Mouse"),
        "Wireless Mouse MX Master 2S"
    );
    assert_eq!(preferred_direct_codename(None, "Mouse"), "Mouse");
}

#[test]
fn cache_dirty_tracks_only_persistable_keys() {
    // A system whose devices never persist (direct-only, or Unifying) must not
    // rewrite probe-cache.json on every refresh pass: the file's content
    // wouldn't change.
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let unifying = CacheKey::UnifyingSlot {
        receiver_uid: "DA2699E1".into(),
        slot: 1,
    };
    e.apply_outcomes(vec![CacheOutcome::Fresh(unifying.clone(), cache_entry())]);
    assert!(
        !e.cache_dirty,
        "non-persistable fresh probe dirtied the cache"
    );

    // Its eviction is equally invisible to the persisted file.
    let nobody = HashSet::new();
    for _ in 0..=CACHE_MISS_GRACE {
        e.evict_unseen(&nobody, &nobody);
    }
    assert!(!e.cache.contains_key(&unifying), "entry should be evicted");
    assert!(!e.cache_dirty, "non-persistable eviction dirtied the cache");

    // A Bolt probe is what the file stores — that one dirties it.
    let bolt = CacheKey::Bolt {
        unit_id: [1, 2, 3, 4],
    };
    e.apply_outcomes(vec![CacheOutcome::Fresh(bolt, cache_entry())]);
    assert!(
        e.cache_dirty,
        "persistable fresh probe must dirty the cache"
    );
}

#[test]
fn cache_entry_survives_grace_then_evicts() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let key = CacheKey::Bolt {
        unit_id: [1, 2, 3, 4],
    };
    e.cache.insert(key.clone(), cache_entry());
    let nobody = HashSet::new();
    // Missing for the whole grace window: kept.
    for _ in 0..CACHE_MISS_GRACE {
        e.evict_unseen(&nobody, &nobody);
        assert!(
            e.cache.contains_key(&key),
            "evicted inside the grace window"
        );
    }
    // One miss past the grace: evicted.
    e.evict_unseen(&nobody, &nobody);
    assert!(
        !e.cache.contains_key(&key),
        "should evict past the grace window"
    );
}

#[test]
fn being_seen_resets_the_miss_counter() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let key = CacheKey::Bolt { unit_id: [9; 4] };
    e.cache.insert(key.clone(), cache_entry());
    let nobody = HashSet::new();
    let seen: HashSet<CacheKey> = std::iter::once(key.clone()).collect();
    e.evict_unseen(&nobody, &nobody); // miss 1
    e.evict_unseen(&seen, &nobody); // seen → counter reset
    for _ in 0..CACHE_MISS_GRACE {
        e.evict_unseen(&nobody, &nobody);
    }
    assert!(
        e.cache.contains_key(&key),
        "counter reset by a sighting, so still within grace"
    );
}

#[test]
fn cached_probe_is_reused_until_refresh_interval() {
    let probed_at = Instant::now();
    let cached = Cached {
        probe: ProbedFeatures::default(),
        battery: None,
        events: EventFeatureIndices::default(),
        probed_at,
    };
    assert!(!is_stale(&cached, probed_at), "same instant is fresh");
    assert!(
        !is_stale(&cached, probed_at + Duration::from_secs(29)),
        "just under the window is still fresh"
    );
    assert!(
        is_stale(&cached, probed_at + REFRESH_INTERVAL),
        "at the window the probe is refreshed"
    );
}

#[test]
fn unifying_cache_hits_use_only_the_battery_refresh_budget() {
    let cached = cache_entry();
    let timeouts = &ProbeTimeouts::DEFAULT;
    assert_eq!(
        unifying_probe_budget(Some(&cached), cached.probed_at, timeouts),
        UNIFYING_CACHED_SLOT_PROBE_TIMEOUT
    );
    assert_eq!(
        unifying_probe_budget(Some(&cached), cached.probed_at + REFRESH_INTERVAL, timeouts),
        UNIFYING_SLOT_PROBE_TIMEOUT,
        "stale entries still get enough time for a full feature walk"
    );
    assert_eq!(
        unifying_probe_budget(None, Instant::now(), timeouts),
        UNIFYING_SLOT_PROBE_TIMEOUT,
        "first sight still gets the full feature-walk budget"
    );
}

#[test]
fn live_cached_channel_survives_a_transient_enumeration_gap() {
    let enumerated = std::collections::HashSet::from([1_u8]);
    let cached_channels = [(1_u8, true), (2_u8, true), (3_u8, false)];
    let retained = retained_nodes(&enumerated, cached_channels);
    assert!(retained.contains(&1));
    assert!(retained.contains(&2));
    assert!(!retained.contains(&3));
    assert_eq!(retained, std::collections::HashSet::from([1, 2]));
}

#[tokio::test]
async fn successful_channel_open_resets_eviction_but_not_inventory_grace() {
    let info = scripted_node_info("replacement");
    let node = info.id.clone();
    let backend = ScriptedBackend::new(vec![(info.clone(), ScriptedOpen::UnresponsiveHidpp)]);
    let mut enumerator = Enumerator::with_backend(backend.clone());
    enumerator
        .ledger
        .settle(&node, true, Some(inventory(&[1]).remove(0)));
    let mut expired = None;
    for _ in 0..8 {
        expired = enumerator.ledger.settle(&node, false, None).inventory;
    }
    assert!(
        expired.is_none(),
        "retirement ticks must eventually stop publishing stale inventory"
    );

    let prepared = enumerator.prepare_nodes(backend.as_ref(), vec![info]).await;
    assert_eq!(prepared.active.len(), 1, "the replacement must open");
    let first_incomplete_probe = enumerator.ledger.settle_arrival_replay_failure(&node);

    assert!(!first_incomplete_probe.evict_channel);
    assert!(first_incomplete_probe.inventory.is_none());
}

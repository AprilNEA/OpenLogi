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
        !is_stale(&cached, probed_at + REFRESH_INTERVAL),
        "the window is inclusive: this exact age is still reused"
    );
    assert!(
        is_stale(
            &cached,
            probed_at + REFRESH_INTERVAL + Duration::from_nanos(1)
        ),
        "the probe is stale only after the window"
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
        UNIFYING_CACHED_SLOT_PROBE_TIMEOUT,
        "an entry aged exactly one refresh interval is still a cache hit"
    );
    assert_eq!(
        unifying_probe_budget(
            Some(&cached),
            cached.probed_at + REFRESH_INTERVAL + Duration::from_nanos(1),
            timeouts,
        ),
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

/// A backend whose `open_hidpp` sleeps for a fixed delay before failing,
/// standing in for a slow BLE-direct connection handshake.
struct SlowOpenBackend {
    delay: Duration,
}

#[hidpp::async_trait]
impl crate::backend::HidBackend for SlowOpenBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
        Ok(Vec::new())
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
        Ok(Vec::new())
    }

    async fn open_hidpp(
        &self,
        _node: &NodeInfo,
    ) -> Result<Option<Arc<hidpp::channel::HidppChannel>>, crate::backend::BackendError> {
        tokio::time::sleep(self.delay).await;
        Err(crate::backend::BackendError::Disconnected)
    }

    async fn open_raw_writer(
        &self,
        _node: &NodeInfo,
    ) -> Result<Box<dyn crate::backend::RawWriter>, crate::backend::BackendError> {
        Err(crate::backend::BackendError::Backend(
            "slow-open test backend has no raw writer".into(),
        ))
    }

    fn watch(&self) -> Result<crate::backend::HotplugStream, crate::backend::BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

/// BLE-direct devices measured 10-15s each to open, fully serialized, because
/// `prepare_nodes` opened one node at a time. Three nodes that each take
/// `DELAY` to open must finish in about one `DELAY`, not three.
///
/// Uses paused virtual time instead of a wall-clock ceiling: `SlowOpenBackend`
/// sleeps via `tokio::time::sleep`, so with time paused a serialized open
/// would need the runtime to auto-advance past three delays before this task
/// can resume, while a concurrent one only needs one — proving the property
/// directly rather than measuring scheduler latency, which a loaded CI
/// runner can blow through even when every open genuinely ran concurrently.
#[tokio::test(start_paused = true)]
async fn opening_multiple_nodes_does_not_serialize_a_slow_connection_handshake() {
    const DELAY: Duration = Duration::from_millis(150);

    let backend = Arc::new(SlowOpenBackend { delay: DELAY });
    let mut enumerator = Enumerator::with_backend(backend.clone());
    let candidates = vec![
        scripted_node_info("slow-a"),
        scripted_node_info("slow-b"),
        scripted_node_info("slow-c"),
    ];

    let start = tokio::time::Instant::now();
    let prepared = tokio::time::timeout(DELAY * 2, enumerator.prepare_nodes(&*backend, candidates))
        .await
        .expect("three concurrent opens must resolve within two delays of virtual time");
    let elapsed = start.elapsed();

    assert_eq!(
        prepared.open_failures.len(),
        3,
        "every node's open must still be attempted and reported"
    );
    assert!(
        elapsed < DELAY * 2,
        "opening three {DELAY:?}-latency nodes advanced virtual time by {elapsed:?} — they were serialized"
    );
}

/// A backend can report the same node twice in one enumeration pass (a
/// duplicate hotplug event, a receiver re-listing a slot). The cache stays
/// empty for every occurrence of a not-yet-cached node until the concurrent
/// opens finish, so without deduplicating `to_open` the duplicate would be
/// opened a second time and end up live through two channels — the exact
/// split-delivery state `ChannelCache` exists to prevent.
#[tokio::test]
async fn a_duplicate_candidate_opens_only_one_channel() {
    let info = scripted_node_info("duplicated");
    let backend = ScriptedBackend::new(vec![(info.clone(), ScriptedOpen::UnresponsiveHidpp)]);
    let mut enumerator = Enumerator::with_backend(backend.clone());

    let prepared = enumerator
        .prepare_nodes(backend.as_ref(), vec![info.clone(), info])
        .await;

    assert_eq!(
        prepared.active.len(),
        1,
        "a duplicate candidate must only ever open one live channel"
    );
}

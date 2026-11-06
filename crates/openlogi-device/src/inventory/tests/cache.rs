//! The probe cache: what it keys, when an entry ages out, and when a cached answer is reused.
use super::*;
use crate::channel::scripted::{feature_error, ping_reply};

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
        walked_at: probed_at,
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

/// A backend whose single node opens into the channel that `open` builds. The
/// channel always reports itself connected, whatever it does with requests.
struct DirectChannelBackend {
    node: NodeInfo,
    open: fn() -> ScriptedRawHidChannel,
}

#[hidpp::async_trait]
impl crate::backend::HidBackend for DirectChannelBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
        Ok(vec![self.node.clone()])
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
        self.enumerate().await
    }

    async fn open_hidpp(
        &self,
        _node: &NodeInfo,
    ) -> Result<Option<Arc<hidpp::channel::HidppChannel>>, crate::backend::BackendError> {
        Ok(Some(scripted_channel((self.open)()).await))
    }

    async fn open_raw_writer(
        &self,
        _node: &NodeInfo,
    ) -> Result<Box<dyn crate::backend::RawWriter>, crate::backend::BackendError> {
        Err(crate::backend::BackendError::Backend(
            "direct-channel test backend has no raw writer".into(),
        ))
    }

    fn watch(&self) -> Result<crate::backend::HotplugStream, crate::backend::BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

/// A handle that looks alive yet can no longer reach the device: it rejects
/// every write.
fn rejecting_writes() -> ScriptedRawHidChannel {
    ScriptedRawHidChannel::with_failing_writes(|_| None, |_| true).0
}

/// A device that answers the protocol ping and fails every other request, as
/// a lossy link presents a walk whose replies go missing.
fn answering_only_its_ping() -> ScriptedRawHidChannel {
    ScriptedRawHidChannel::with_responder(|request| {
        ping_reply(request).or_else(|| Some(feature_error(request, 0x01)))
    })
    .0
}

/// A due cache entry for a pointer device whose last walk completed.
fn stale_pointer_entry() -> Cached {
    let mut last_good = cache_entry();
    last_good.probe.protocol = ProbedProtocol::Hidpp20 {
        capabilities: Capabilities {
            pointer: true,
            ..Capabilities::default()
        },
        incomplete: false,
    };
    last_good.probed_at = Instant::now()
        .checked_sub(REFRESH_INTERVAL + Duration::from_nanos(1))
        .expect("the refresh interval fits the monotonic clock");
    last_good
}

/// A stale direct-device probe whose refresh fails is a failed probe, not a
/// cache hit. Serving the cached answer in its place reports an unreachable
/// channel healthy, so the ledger never retires it and the device stays dead
/// until the agent restarts. A silent device takes the same branch once its
/// request times out; rejected writes reach it without waiting.
#[tokio::test]
async fn a_failed_direct_reprobe_retires_the_channel() {
    let info = scripted_node_info("unreachable-direct");
    let node = info.id.clone();
    let backend = Arc::new(DirectChannelBackend {
        node: info,
        open: rejecting_writes,
    });
    let mut enumerator = Enumerator::with_backend(backend);
    enumerator
        .cache
        .insert(CacheKey::Direct(node.clone()), stale_pointer_entry());

    enumerator
        .enumerate()
        .await
        .expect("a failed reprobe still settles");
    assert!(
        !enumerator.channels.is_retiring(&node),
        "one failed reprobe is not yet evidence of a dead channel"
    );
    enumerator
        .enumerate()
        .await
        .expect("a failed reprobe still settles");
    assert!(
        enumerator.channels.is_retiring(&node),
        "a second failed reprobe must retire the channel so it reopens"
    );
}

/// A stale direct-device probe whose device answers the ping but not the
/// re-read keeps its channel and stays published. The ping shows that the
/// channel delivers replies. Retiring it for a re-read that a lossy Bluetooth
/// link cut short ends control capture on a working device (#2107).
#[tokio::test]
async fn a_direct_reprobe_answered_by_its_ping_keeps_the_channel() {
    let info = scripted_node_info("lossy-direct");
    let node = info.id.clone();
    let backend = Arc::new(DirectChannelBackend {
        node: info,
        open: answering_only_its_ping,
    });
    let mut enumerator = Enumerator::with_backend(backend);
    enumerator
        .cache
        .insert(CacheKey::Direct(node.clone()), stale_pointer_entry());

    // Past the ledger's retirement threshold and its replay grace both.
    for pass in 1..=4 {
        let inventories = enumerator
            .enumerate()
            .await
            .expect("a reprobe the device answered settles");
        assert!(
            !enumerator.channels.is_retiring(&node),
            "pass {pass}: a channel that answers its ping is not dead"
        );
        assert_eq!(
            inventories.len(),
            1,
            "pass {pass}: the device stays published from its last complete walk"
        );
    }
    assert!(
        enumerator
            .cache
            .get(&CacheKey::Direct(node))
            .is_some_and(|entry| entry.probe.capabilities().is_some()),
        "the unfinished re-read must not replace the complete walk"
    );
}

const RETURNING_UNIT: [u8; 4] = [0x55, 0xe9, 0xc6, 0xba];
const RETURNING_MODELS: [u16; 3] = [0xb023, 0x4082, 0];

/// A completed walk of a pointer device whose `0x0003` reported `unit_id`.
fn pointer_walk_of(unit_id: [u8; 4]) -> Cached {
    let mut walk = cache_entry();
    walk.probe.protocol = ProbedProtocol::Hidpp20 {
        capabilities: Capabilities {
            pointer: true,
            scroll_inversion: true,
            ..Capabilities::default()
        },
        incomplete: false,
    };
    walk.probe.model_info = Some(DeviceModelInfo {
        entity_count: 3,
        serial_number: None,
        unit_id,
        transports: DeviceTransports::default(),
        model_ids: RETURNING_MODELS,
        extended_model_id: 0,
    });
    walk.probe.table_head = Some(TableHead::new(1, RETURNING_TABLE_LEN));
    walk
}

/// How many features the returning unit's table lists, FeatureSet at 1.
const RETURNING_TABLE_LEN: u8 = 30;

/// A HID++ 2.0 long reply to `request` carrying `payload`.
fn long_reply(request: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut reply = vec![0u8; 20];
    reply[0] = 0x11;
    reply[1..4].copy_from_slice(&request[1..4]);
    reply[4..4 + payload.len()].copy_from_slice(payload);
    reply
}

/// The returning unit's `0x0003`, at feature index 2, answering only its
/// identity read and its table's head: every other request fails, so a
/// feature-table walk can never complete against it and only a reused walk
/// can publish it.
fn answering_only_its_identity() -> ScriptedRawHidChannel {
    answering_its_identity_listing(RETURNING_TABLE_LEN)
}

/// The returning unit, back with a table of `count` features: what it lists
/// over another transport, or after a firmware update.
fn answering_with_another_table() -> ScriptedRawHidChannel {
    answering_its_identity_listing(RETURNING_TABLE_LEN + 1)
}

fn answering_its_identity_listing(count: u8) -> ScriptedRawHidChannel {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        if let Some(pong) = ping_reply(request) {
            return Some(pong);
        }
        let function = request[3] >> 4;
        match (request[2], function, &request[4..6]) {
            (0x00, 0, [0x00, 0x01]) => Some(long_reply(request, &[0x01, 0x00, 0x00])),
            (0x01, 0, _) => Some(long_reply(request, &[count])),
            (0x00, 0, [0x00, 0x03]) => Some(long_reply(request, &[0x02, 0x00, 0x04])),
            (0x02, 0, _) => {
                let [m0, m1, m2] = RETURNING_MODELS.map(u16::to_be_bytes);
                let mut info = vec![3];
                info.extend_from_slice(&RETURNING_UNIT);
                info.extend_from_slice(&[0x00, 0x00]);
                info.extend_from_slice(&[m0[0], m0[1], m1[0], m1[1], m2[0], m2[1]]);
                Some(long_reply(request, &info))
            }
            _ => Some(feature_error(request, 0x02)),
        }
    })
    .0
}

/// One pass over a node that was never probed, with `prepare` setting up the
/// enumerator first; returns the enumerator and the pass's inventories.
async fn first_pass_on_a_new_node(
    node: &NodeInfo,
    prepare: impl FnOnce(&mut Enumerator),
) -> (Enumerator, Vec<DeviceInventory>) {
    first_pass_answered_by(node, answering_only_its_identity, prepare).await
}

async fn first_pass_answered_by(
    node: &NodeInfo,
    open: fn() -> ScriptedRawHidChannel,
    prepare: impl FnOnce(&mut Enumerator),
) -> (Enumerator, Vec<DeviceInventory>) {
    let backend = Arc::new(DirectChannelBackend {
        node: node.clone(),
        open,
    });
    let mut enumerator = Enumerator::with_backend(backend);
    prepare(&mut enumerator);
    let inventories = enumerator.enumerate().await.expect("a direct node settles");
    (enumerator, inventories)
}

fn published_capabilities(inventories: &[DeviceInventory]) -> Option<Capabilities> {
    inventories
        .iter()
        .flat_map(|inventory| &inventory.paired)
        .find_map(|device| device.capabilities)
}

/// A Bluetooth device that powers back on returns on a new OS node. Its walk
/// was evicted with the old node, and must still serve it after the identity
/// read proves the unit, instead of a full table walk before it can be
/// published and its volatile settings re-applied.
#[tokio::test]
async fn a_direct_device_returning_on_a_new_node_reuses_its_departed_walk() {
    let node = scripted_node_info("returned-direct");
    let (enumerator, inventories) = first_pass_on_a_new_node(&node, |enumerator| {
        let old = CacheKey::Direct(NodeId::from("departed-direct".to_owned()));
        enumerator
            .cache
            .insert(old, pointer_walk_of(RETURNING_UNIT));
        let nobody = HashSet::new();
        for _ in 0..=CACHE_MISS_GRACE {
            enumerator.evict_unseen(&nobody, &nobody);
        }
        assert!(enumerator.cache.is_empty(), "the old node's entry aged out");
    })
    .await;

    assert!(
        published_capabilities(&inventories).is_some_and(|caps| caps.scroll_inversion),
        "the returning unit is published from its departed walk: {inventories:?}"
    );
    assert!(
        enumerator
            .cache
            .get(&CacheKey::Direct(node.id.clone()))
            .is_some_and(|entry| UnitIdentity::of(&entry.probe)
                == UnitIdentity::new(RETURNING_UNIT, RETURNING_MODELS)),
        "the walk moves under the new node's key, so the next pass hits it"
    );
    assert_eq!(
        enumerator.returned_direct_devices_last_tick(),
        [DeviceRoute::Direct {
            vendor_id: node.vendor_id,
            product_id: node.product_id,
        }],
        "its return is reported, so its device-RAM settings are re-applied"
    );
}

/// The same unit back over another transport, or after a firmware update,
/// lists another table: its old walk would publish the wrong capabilities
/// and feature indexes, so it is walked instead.
#[tokio::test]
async fn a_unit_returning_with_another_table_does_not_reuse_its_walk() {
    let node = scripted_node_info("returned-over-usb");
    let (enumerator, inventories) =
        first_pass_answered_by(&node, answering_with_another_table, |enumerator| {
            enumerator.departed.keep(pointer_walk_of(RETURNING_UNIT));
        })
        .await;

    assert!(
        published_capabilities(&inventories).is_none(),
        "the old walk does not publish it: {inventories:?}"
    );
    assert!(enumerator.returned_direct_devices_last_tick().is_empty());
}

/// A device back on a new node in the same pass its old node vanished is
/// never published offline, so its return is reported once — and only once:
/// the next pass finds it where it was.
#[tokio::test]
async fn a_return_is_reported_for_the_pass_that_found_it_only() {
    let node = scripted_node_info("swapped-direct");
    let (mut enumerator, _) = first_pass_on_a_new_node(&node, |enumerator| {
        let old = CacheKey::Direct(NodeId::from("swapped-out-direct".to_owned()));
        enumerator
            .cache
            .insert(old, pointer_walk_of(RETURNING_UNIT));
    })
    .await;
    assert_eq!(enumerator.returned_direct_devices_last_tick().len(), 1);

    enumerator
        .enumerate()
        .await
        .expect("the returned node settles again");
    assert!(enumerator.returned_direct_devices_last_tick().is_empty());
}

/// A reconnect inside the old node's eviction grace finds the walk still
/// cached under that node.
#[tokio::test]
async fn a_direct_device_returning_inside_the_grace_reuses_its_cached_walk() {
    let node = scripted_node_info("quickly-returned-direct");
    let (_, inventories) = first_pass_on_a_new_node(&node, |enumerator| {
        let old = CacheKey::Direct(NodeId::from("previous-direct".to_owned()));
        enumerator
            .cache
            .insert(old, pointer_walk_of(RETURNING_UNIT));
    })
    .await;

    assert!(
        published_capabilities(&inventories).is_some(),
        "the returning unit is published from the old node's entry"
    );
}

/// Another unit of the same model must not inherit the walk: an identity that
/// matches nothing gets the full probe.
#[tokio::test]
async fn another_unit_on_a_new_node_does_not_reuse_a_walk() {
    let node = scripted_node_info("other-unit-direct");
    let (enumerator, inventories) = first_pass_on_a_new_node(&node, |enumerator| {
        enumerator
            .departed
            .keep(pointer_walk_of([0x01, 0x02, 0x03, 0x04]));
    })
    .await;

    assert!(
        published_capabilities(&inventories).is_none(),
        "a walk that is not the unit's own must not publish it: {inventories:?}"
    );
    assert!(enumerator.returned_direct_devices_last_tick().is_empty());
}

/// Only a walk that read a unit id telling units apart can be matched, so a
/// new node with none to match skips the identity read.
#[test]
fn a_reusable_walk_needs_a_unit_identity() {
    let mut departed = DepartedUnits::default();
    departed.keep(pointer_walk_of([0; 4]));
    let mut cache = HashMap::new();
    cache.insert(
        CacheKey::Direct(NodeId::from("no-device-information".to_owned())),
        stale_pointer_entry(),
    );
    cache.insert(
        CacheKey::Direct(NodeId::from("all-zero-unit".to_owned())),
        pointer_walk_of([0; 4]),
    );
    let walks = DirectWalks {
        cache: &cache,
        departed: &departed,
    };
    assert!(
        !walks.any(),
        "neither a walk without 0x0003 nor an all-zero unit id keys anything"
    );
}

/// A unit keeps one departed walk, its latest, and the store stays bounded.
#[test]
fn departed_walks_keep_the_latest_per_unit_within_capacity() {
    let mut departed = DepartedUnits::default();
    let older = pointer_walk_of(RETURNING_UNIT);
    let mut newer = pointer_walk_of(RETURNING_UNIT);
    newer.probed_at = older.probed_at + Duration::from_secs(1);
    departed.keep(older);
    departed.keep(newer.clone());
    for unit in 1..=40u8 {
        let mut other = pointer_walk_of([unit, 0, 0, 1]);
        other.probed_at = newer.probed_at + Duration::from_secs(u64::from(unit));
        departed.keep(other);
    }
    let cache = HashMap::new();
    let walks = DirectWalks {
        cache: &cache,
        departed: &departed,
    };
    let identity = UnitIdentity::new([40, 0, 0, 1], RETURNING_MODELS).expect("nonzero unit");
    assert!(walks.of_unit(identity).is_some(), "the latest unit is kept");
    let evicted = UnitIdentity::new(RETURNING_UNIT, RETURNING_MODELS).expect("nonzero unit");
    assert!(
        walks.of_unit(evicted).is_none(),
        "the least recently probed unit makes room at capacity"
    );
}

/// A device answering its ping and a table head of FeatureSet at index 1
/// listing 30 features; every other request fails, so no walk can finish.
fn answering_its_table_head() -> ScriptedRawHidChannel {
    ScriptedRawHidChannel::with_responder(|request| {
        if let Some(pong) = ping_reply(request) {
            return Some(pong);
        }
        let mut reply = request[..4].to_vec();
        match (request[2], request[3] >> 4, request[4], request[5]) {
            (0x00, 0x00, 0x00, 0x01) => reply.extend_from_slice(&[0x01, 0x00, 0x00]),
            (0x01, 0x00, _, _) => reply.extend_from_slice(&[30, 0x00, 0x00]),
            _ => return Some(feature_error(request, 0x02)),
        }
        Some(reply)
    })
    .0
}

/// A due refresh whose table head still matches serves the cached walk as
/// the device's live answer and restarts the refresh clock, so the next pass
/// is a plain cache hit. The walk itself keeps its age, for the periodic
/// re-walk.
#[tokio::test]
async fn a_due_refresh_with_an_unchanged_table_head_restarts_only_the_refresh_clock() {
    let channel = scripted_channel(answering_its_table_head()).await;
    let mut due = stale_pointer_entry();
    due.probe.table_head = Some(TableHead::new(1, 30));
    let walked_at = due.walked_at;
    let key = CacheKey::Direct(NodeId::from("unchanged-head".to_owned()));
    let now = Instant::now();

    let (probe, outcome) = super::super::cache::probe_or_reuse(
        &channel,
        super::super::cache::ProbeTarget {
            index: DIRECT_DEVICE_INDEX,
            registers: None,
        },
        Some(key.clone()),
        Some(&due),
        true,
        now,
        None,
    )
    .await;

    assert!(probe.capabilities().is_some_and(|caps| caps.pointer));
    let CacheOutcome::Update(updated_key, entry) = outcome else {
        panic!("the refreshed walk goes back into the cache");
    };
    assert_eq!(updated_key, key);
    assert_eq!(entry.probed_at, now);
    assert_eq!(entry.walked_at, walked_at);
}

/// However well its head matches, a walk is taken again once it is older
/// than the re-walk interval, for a firmware change the head cannot show.
#[test]
fn an_old_walk_is_due_again_whatever_its_head() {
    let mut walk = cache_entry();
    walk.probe.table_head = Some(TableHead::new(1, 30));
    let walked = walk.walked_at;
    assert!(walk.known_table(walked + TABLE_REWALK_INTERVAL).is_some());
    assert!(
        walk.known_table(walked + TABLE_REWALK_INTERVAL + Duration::from_nanos(1))
            .is_none()
    );
    walk.probe.table_head = None;
    assert!(
        walk.known_table(walked).is_none(),
        "a walk loaded from disk has no head to compare"
    );
}

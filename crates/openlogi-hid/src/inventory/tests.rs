use std::collections::HashSet;
use std::sync::Arc;

use openlogi_core::device::{
    DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
};

use super::cache::{
    CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached, REFRESH_TICKS, backfill_identity, is_stale,
};
use super::persist;
use super::probe::{
    NodeProbe, assemble_bolt_probe, parse_codename_unifying, preferred_direct_codename,
};
use super::{
    ChannelCache, Enumerator, ONESHOT_ATTEMPTS, one_shot_should_stop, retained_nodes,
    routes_for_inventories, settle_unhealthy_node,
};
use crate::inventory::features::{BatteryProbe, ProbedFeatures};
use crate::{DIRECT_DEVICE_INDEX, DeviceRoute};

fn cache_entry(probed_tick: u64) -> Cached {
    Cached {
        probe: ProbedFeatures::default(),
        battery: None,
        probed_tick,
    }
}

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
    let mut e = Enumerator::default();
    let unifying = CacheKey::UnifyingSlot {
        receiver_uid: "DA2699E1".into(),
        slot: 1,
    };
    e.apply_outcomes(vec![CacheOutcome::Fresh(unifying.clone(), cache_entry(0))]);
    assert!(
        !e.cache_dirty,
        "non-persistable fresh probe dirtied the cache"
    );

    // Its eviction is equally invisible to the persisted file.
    let nobody = HashSet::new();
    for _ in 0..=CACHE_MISS_GRACE {
        e.evict_unseen(&nobody);
    }
    assert!(!e.cache.contains_key(&unifying), "entry should be evicted");
    assert!(!e.cache_dirty, "non-persistable eviction dirtied the cache");

    // A Bolt probe is what the file stores — that one dirties it.
    let bolt = CacheKey::Bolt {
        unit_id: [1, 2, 3, 4],
    };
    e.apply_outcomes(vec![CacheOutcome::Fresh(bolt, cache_entry(0))]);
    assert!(
        e.cache_dirty,
        "persistable fresh probe must dirty the cache"
    );
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
        battery: None,
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

fn inventory(slots: &[u8]) -> Vec<DeviceInventory> {
    vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "Unifying Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc52b,
            unique_id: Some("receiver-1".to_string()),
        },
        paired: slots
            .iter()
            .copied()
            .map(|slot| PairedDevice {
                slot,
                codename: Some(format!("device-{slot}")),
                wpid: Some(0xb000 + u16::from(slot)),
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: None,
            })
            .collect(),
    }]
}

#[test]
fn settled_inventories_publish_exact_receiver_routes() {
    assert_eq!(
        routes_for_inventories(&inventory(&[1, 4])),
        vec![
            DeviceRoute::Unifying {
                receiver_uid: "receiver-1".into(),
                slot: 1,
            },
            DeviceRoute::Unifying {
                receiver_uid: "receiver-1".into(),
                slot: 4,
            },
        ]
    );

    assert_eq!(
        routes_for_inventories(&inventory(&[4])),
        vec![DeviceRoute::Unifying {
            receiver_uid: "receiver-1".into(),
            slot: 4,
        }],
        "a vanished slot must not survive the next atomic node replacement"
    );
}

#[test]
fn settled_direct_inventory_publishes_one_direct_route() {
    let direct = vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Keys".into(),
            vendor_id: 0x046d,
            product_id: 0xb35b,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Keys".into()),
            wpid: Some(0xb35b),
            kind: DeviceKind::Keyboard,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        }],
    }];

    assert_eq!(
        routes_for_inventories(&direct),
        vec![DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        }]
    );
}

#[test]
fn channel_cache_retires_and_defers_reopen_until_a_later_tick() {
    let mut cache = ChannelCache::<u8, Arc<()>>::default();
    let channel = Arc::new(());
    cache.insert(1, Arc::clone(&channel));

    assert!(cache.retire_node(&1).is_some());
    assert!(cache.get(&1).is_none());
    assert!(!cache.prepare_open(&1, |channel| Arc::strong_count(channel) == 1));

    drop(channel);
    assert!(cache.is_retiring(&1));
    assert!(
        !cache.prepare_open(&1, |channel| Arc::strong_count(channel) == 1),
        "the tick that drops retirement still skips opening"
    );
    assert!(!cache.is_retiring(&1));
    assert!(
        cache.prepare_open(&1, |channel| Arc::strong_count(channel) == 1),
        "only a later tick may reopen"
    );
}

#[test]
fn absent_channels_retire_and_quiescent_absent_retirement_is_reaped() {
    let mut cache = ChannelCache::<u8, Arc<()>>::default();
    cache.insert(1, Arc::new(()));
    cache.insert(2, Arc::new(()));

    cache.retire_absent(&HashSet::from([2]));
    assert!(cache.is_retiring(&1));
    assert!(cache.get(&2).is_some());

    cache.reap_absent(&HashSet::from([2]), |channel| {
        Arc::strong_count(channel) == 1
    });
    assert!(!cache.is_retiring(&1));
}

#[test]
fn retiring_node_replays_ledger_and_marks_tick_unhealthy() {
    let mut ledger = crate::node_ledger::NodeLedger::<u8>::default();
    let expected = inventory(&[1]);
    let settled = ledger.settle(&1, true, Some(expected[0].clone()));
    assert_eq!(settled.inventory, Some(expected[0].clone()));

    let mut complete = true;
    let mut healthy = true;
    let replay = settle_unhealthy_node(&mut ledger, &1, &mut complete, &mut healthy);

    assert_eq!(replay, Some(expected[0].clone()));
    assert!(!complete);
    assert!(!healthy);
}

#[test]
fn retiring_node_inventory_expires_after_the_existing_ledger_grace() {
    let mut ledger = crate::node_ledger::NodeLedger::<u8>::default();
    let expected = inventory(&[1]);
    ledger.settle(&1, true, Some(expected[0].clone()));

    let mut complete = true;
    let mut healthy = true;
    for _ in 0..3 {
        assert_eq!(
            settle_unhealthy_node(&mut ledger, &1, &mut complete, &mut healthy),
            Some(expected[0].clone())
        );
    }
    assert_eq!(
        settle_unhealthy_node(&mut ledger, &1, &mut complete, &mut healthy),
        None,
        "retirement must not extend stale inventory beyond ledger policy"
    );
}

#[test]
fn one_shot_retry_stops_when_first_attempt_is_complete() {
    let current = inventory(&[1, 2]);

    assert!(
        one_shot_should_stop(None, &current, true, true, 1),
        "complete inventories keep the one-pass happy path"
    );
}

#[test]
fn one_shot_retry_waits_for_healthy_incomplete_inventory_to_stabilize() {
    let partial = inventory(&[1]);
    let full = inventory(&[1, 2]);

    assert!(
        !one_shot_should_stop(None, &partial, false, true, 1),
        "the first incomplete pass has no previous inventory to compare"
    );
    assert!(
        !one_shot_should_stop(Some(partial.as_slice()), &full, false, true, 2),
        "a changed inventory should get another retry window"
    );
    assert!(
        one_shot_should_stop(Some(full.as_slice()), &full, false, true, 3),
        "once the returned inventory stabilizes, retrying stops"
    );
}

#[test]
fn one_shot_retry_stops_on_unchanged_incomplete_inventory() {
    let partial = inventory(&[1]);

    assert!(
        one_shot_should_stop(Some(partial.as_slice()), &partial, false, true, 2),
        "stable partial inventories should not burn every retry attempt"
    );
}

#[test]
fn one_shot_retry_keeps_unchanged_inventory_after_unhealthy_probe() {
    let partial = inventory(&[1]);

    assert!(
        !one_shot_should_stop(Some(partial.as_slice()), &partial, false, false, 2),
        "unchanged replay after a failed probe must keep retrying before the cap"
    );
}

#[test]
fn one_shot_retry_stops_at_attempt_cap_when_inventory_keeps_changing() {
    let previous = inventory(&[1]);
    let current = inventory(&[1, 2]);

    assert!(
        one_shot_should_stop(
            Some(previous.as_slice()),
            &current,
            false,
            false,
            ONESHOT_ATTEMPTS
        ),
        "the retry loop must remain bounded even if the inventory changes every time"
    );
}

fn bolt_receiver_info() -> ReceiverInfo {
    ReceiverInfo {
        name: "Logi Bolt Receiver".to_string(),
        vendor_id: 0x046d,
        product_id: 0xc548,
        unique_id: Some("bolt-1".to_string()),
    }
}

/// A readable slot's probe result. `Seen` models the fallback a feature-walk
/// timeout produces (#251): the device still surfaces from its pairing-register
/// identity, so a timed-out slot counts as readable here.
fn bolt_slot(slot: u8) -> (PairedDevice, CacheOutcome) {
    (
        PairedDevice {
            slot,
            codename: Some(format!("device-{slot}")),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        },
        CacheOutcome::Seen(CacheKey::Bolt {
            unit_id: [0, 0, 0, slot],
        }),
    )
}

fn paired_slots(probe: &NodeProbe) -> Vec<u8> {
    let Some(inventory) = probe.inventory.as_ref() else {
        panic!("expected an inventory");
    };
    inventory.paired.iter().map(|d| d.slot).collect()
}

#[test]
fn bolt_probe_is_complete_when_count_matches_readable_slots() {
    // Two paired slots, both readable, and the pairing-count register agrees.
    // Empty slots are dropped in phase 1, so only occupied slots reach here;
    // `join` yields them in slot order, so the devices must come out ordered
    // without an explicit sort.
    let probe = assemble_bolt_probe(
        bolt_receiver_info(),
        Some(2),
        vec![bolt_slot(1), bolt_slot(2)],
    );
    assert!(probe.complete, "count matches the readable slots");
    assert!(probe.healthy, "a complete Bolt walk is authoritative");
    assert_eq!(paired_slots(&probe), vec![1, 2], "slots surface in order");
    assert_eq!(
        probe.outcomes.len(),
        2,
        "one cache outcome per readable slot"
    );
}

#[test]
fn bolt_probe_is_incomplete_when_a_counted_slot_is_unreadable() {
    // The receiver reports two paired devices but only one slot's pairing
    // register read this tick. Presenting that partial walk as the new truth is
    // the #218 regression: it must stay incomplete so the ledger replays the
    // last good snapshot instead of dropping the missing device.
    let probe = assemble_bolt_probe(bolt_receiver_info(), Some(2), vec![bolt_slot(1)]);
    assert_eq!(
        paired_slots(&probe),
        vec![1],
        "only the readable slot surfaces"
    );
    assert!(!probe.complete, "a count shortfall is not complete");
    assert!(
        !probe.healthy,
        "an incomplete Bolt walk is not authoritative"
    );
}

#[test]
fn bolt_probe_is_incomplete_when_the_count_register_is_unanswered() {
    // A parked/unresponsive receiver channel returns no pairing count. Even with
    // slots surfaced from arrival events, the walk can't be trusted as the whole
    // truth, so it stays incomplete and the ledger keeps the prior snapshot.
    let probe = assemble_bolt_probe(bolt_receiver_info(), None, vec![bolt_slot(1), bolt_slot(2)]);
    assert_eq!(paired_slots(&probe), vec![1, 2]);
    assert!(
        !probe.complete,
        "no count register means we couldn't fully check"
    );
    assert!(!probe.healthy);
}

fn model(unit_id: [u8; 4], serial: Option<&str>) -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 1,
        serial_number: serial.map(str::to_string),
        unit_id,
        transports: DeviceTransports::default(),
        model_ids: [0xc09d, 0, 0],
        extended_model_id: 1,
    }
}

fn probed(model_info: Option<DeviceModelInfo>, identity_incomplete: bool) -> ProbedFeatures {
    ProbedFeatures {
        model_info,
        identity_incomplete,
        kind: Some(DeviceKind::Mouse),
        ..ProbedFeatures::default()
    }
}

#[test]
fn failed_device_info_read_backfills_from_cache() {
    let mut fresh = probed(None, true);
    let cached = probed(Some(model([0x46, 0, 0x2e, 0], None)), false);

    backfill_identity(&mut fresh, &cached);

    assert_eq!(fresh.model_info, cached.model_info);
    assert!(
        !fresh.identity_incomplete,
        "a backfilled identity is complete and may be cached"
    );
}

#[test]
fn failed_serial_read_backfills_only_the_serial() {
    let mut fresh = probed(Some(model([1, 2, 3, 4], None)), true);
    let cached = probed(Some(model([9, 9, 9, 9], Some("abc123"))), false);

    backfill_identity(&mut fresh, &cached);

    let Some(info) = fresh.model_info else {
        panic!("model info kept");
    };
    assert_eq!(info.serial_number.as_deref(), Some("abc123"));
    assert_eq!(info.unit_id, [1, 2, 3, 4], "fresh unit id wins");
    assert!(!fresh.identity_incomplete);
}

#[test]
fn complete_probe_is_never_overwritten_by_cache() {
    let mut fresh = probed(Some(model([1, 2, 3, 4], None)), false);
    let cached = probed(Some(model([9, 9, 9, 9], Some("stale"))), false);

    backfill_identity(&mut fresh, &cached);

    let Some(info) = fresh.model_info else {
        panic!("model info kept");
    };
    assert_eq!(info.unit_id, [1, 2, 3, 4]);
    assert!(
        info.serial_number.is_none(),
        "no serial was read, none faked"
    );
}

#[test]
fn incomplete_probe_without_cached_identity_stays_incomplete() {
    let mut fresh = probed(None, true);
    let cached = probed(None, false);

    backfill_identity(&mut fresh, &cached);

    assert!(
        fresh.identity_incomplete,
        "nothing to backfill from — the caller must not memoize this probe"
    );
}

#[test]
fn failed_kind_read_is_carried_forward() {
    let mut fresh = ProbedFeatures::default();
    let cached = probed(None, false);

    backfill_identity(&mut fresh, &cached);

    assert_eq!(fresh.kind, Some(DeviceKind::Mouse));
}

#[test]
fn codename_reads_len_prefixed_name() {
    // wire-verified MX Master 2S reply: `40 0c "MX Master 2S"` then padding.
    let mut buf = vec![0x40, 0x0c];
    buf.extend_from_slice(b"MX Master 2S");
    buf.extend_from_slice(&[0u8; 2]); // trailing bytes of the 16-byte register
    assert_eq!(
        parse_codename_unifying(&buf).as_deref(),
        Some("MX Master 2S")
    );
}

#[test]
fn codename_clamps_overlong_len() {
    // a bogus length byte must not over-read past the buffer.
    let buf = [0x40, 0xff, b'h', b'i'];
    assert_eq!(parse_codename_unifying(&buf).as_deref(), Some("hi"));
}

#[test]
fn codename_rejects_short_response() {
    assert_eq!(parse_codename_unifying(&[0x40]), None);
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

#[test]
fn probe_cache_roundtrips_through_disk() {
    // A device fully probed once must keep its identity across restarts: the
    // persisted cache is what spares a fresh process the expensive (and on
    // degraded transports, failing) re-interview.
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceModelInfo, DeviceTransports,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe-cache.json");

    let model = DeviceModelInfo {
        entity_count: 1,
        serial_number: Some("TESTSERIAL01".into()),
        unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
        transports: DeviceTransports::default(),
        model_ids: [0xb042, 0, 0],
        extended_model_id: 0,
    };
    let probe = ProbedFeatures {
        model_info: Some(model.clone()),
        // A live reading at save time: volatile, so it must NOT survive the
        // round trip (the feature index in `battery` below does).
        battery: Some(BatteryInfo {
            percentage: 55,
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        }),
        ..Default::default()
    };
    let mut cache = std::collections::HashMap::new();
    cache.insert(
        CacheKey::Bolt {
            unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
        },
        Cached {
            probe,
            battery: Some(BatteryProbe::Unified(9)),
            probed_tick: 7,
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
            probed_tick: 3,
        },
    );

    persist::save(&path, &cache).expect("save");
    let loaded = persist::load(&path);

    let bolt = loaded
        .get(&CacheKey::Bolt {
            unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
        })
        .expect("bolt entry survives a save/load cycle");
    assert_eq!(bolt.probe.model_info.as_ref(), Some(&model));
    assert_eq!(bolt.battery, Some(BatteryProbe::Unified(9)));
    assert!(
        bolt.probe.battery.is_none(),
        "the volatile battery reading must not be resurrected across restarts"
    );
    assert_eq!(
        bolt.probed_tick, 0,
        "loaded entries restart the refresh clock"
    );
    assert!(
        !loaded.contains_key(&CacheKey::UnifyingSlot {
            receiver_uid: "DA2699E1".into(),
            slot: 2,
        }),
        "unifying entries are slot-keyed, so a re-pair while the agent is \
         down could hand them to a different device — never persisted"
    );
}

#[test]
fn probe_cache_load_tolerates_missing_or_garbage_files() {
    // The persisted cache is a warm-start optimization: a missing file, torn
    // write, or foreign schema must yield an empty cache, never an error.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope.json");
    assert!(persist::load(&missing).is_empty());

    let garbage = dir.path().join("garbage.json");
    std::fs::write(&garbage, b"not json at all").expect("write");
    assert!(persist::load(&garbage).is_empty());

    let wrong_version = dir.path().join("future.json");
    std::fs::write(&wrong_version, br#"{"version":999,"entries":[]}"#).expect("write");
    assert!(persist::load(&wrong_version).is_empty());
}

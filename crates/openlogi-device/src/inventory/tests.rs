use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hidpp::protocol::v10::{Message, MessageHeader};
use hidpp::receiver::unifying::{Event as UnifyingEvent, decode_notification};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
    ReceiverInfo,
};

use super::cache::{
    CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached, REFRESH_INTERVAL, backfill_identity,
    is_stale, keep_known_capabilities,
};
use super::events::EventFeatureIndices;
use super::features::ProbedFeatures;
use super::probe::{
    NodeProbe, ProbeVerdict, assemble_bolt_probe, assemble_unifying_device,
    parse_codename_unifying, preferred_direct_codename, probe_unifying_slot, unifying_probe_budget,
};
use super::{
    ChannelCache, Enumerator, ONESHOT_ATTEMPTS, OneShotScan, ScanPass, UNIFYING_CACHED_SLOT_PROBE,
    UNIFYING_SLOT_PROBE, retained_nodes, routes_for_inventories, settle_unhealthy_node,
};
use crate::channel::scripted::{
    ScriptedBackend, ScriptedNode, ScriptedRawHidChannel, scripted_channel, scripted_node_info,
};
use crate::{DIRECT_DEVICE_INDEX, DeviceRoute};

fn cache_entry() -> Cached {
    Cached {
        probe: ProbedFeatures::default(),
        battery: None,
        events: EventFeatureIndices::default(),
        probed_at: Instant::now(),
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
        e.evict_unseen(&nobody);
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
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let key = CacheKey::Bolt { unit_id: [9; 4] };
    e.cache.insert(key.clone(), cache_entry());
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
    assert_eq!(
        unifying_probe_budget(Some(&cached), cached.probed_at),
        UNIFYING_CACHED_SLOT_PROBE
    );
    assert_eq!(
        unifying_probe_budget(Some(&cached), cached.probed_at + REFRESH_INTERVAL),
        UNIFYING_SLOT_PROBE,
        "stale entries still get enough time for a full feature walk"
    );
    assert_eq!(
        unifying_probe_budget(None, Instant::now()),
        UNIFYING_SLOT_PROBE,
        "first sight still gets the full feature-walk budget"
    );
}

#[tokio::test]
async fn offline_arrival_rebroadcasts_surface_without_probing_the_device() {
    // The exact wire bytes once misread as proof that the online bit is
    // stuck: `04 62 69 40` is an encrypted MX Master 2S (wpid 0x4069) slot
    // re-broadcast with bit 6 *set* — link not established, device offline.
    let message = Message::Short(
        MessageHeader {
            device_index: 1,
            sub_id: 0x41,
        },
        [0x04, 0x62, 0x69, 0x40],
    );
    let Some(UnifyingEvent::DeviceConnection(event)) = decode_notification(&message) else {
        panic!("expected a device-connection event");
    };
    assert!(!event.online, "bit 6 set must decode as offline");

    let (raw, handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let channel = scripted_channel(raw).await;
    let writes_before = handle.written_reports().len();

    let cache = HashMap::new();
    let (device, _) = probe_unifying_slot(&channel, &event, "SERIAL", &cache, Instant::now(), None)
        .await
        .expect("an offline slot still surfaces from its re-broadcast");

    assert!(!device.online);
    assert_eq!(device.wpid, Some(0x4069));
    assert_eq!(
        handle.written_reports().len(),
        writes_before,
        "an offline slot must not be probed for features, battery, or codename"
    );
}

#[test]
fn unifying_arrival_liveness_survives_missing_feature_data() {
    let device = assemble_unifying_device(
        1,
        None,
        0x40b8,
        DeviceKind::Mouse,
        ProbedFeatures::default(),
        true,
    );
    assert!(device.online);
    assert_eq!(device.wpid, Some(0x40b8));
    assert_eq!(device.kind, DeviceKind::Mouse);
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

    assert!(cache.retire_node(&1));
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

    let retired = cache.retire_absent(&HashSet::from([2]));
    assert_eq!(retired, 1, "one absent channel retires once");
    assert!(cache.is_retiring(&1));
    assert!(cache.get(&2).is_some());

    cache.reap_absent(&HashSet::from([2]), |channel| {
        Arc::strong_count(channel) == 1
    });
    assert!(!cache.is_retiring(&1));
}

#[test]
fn retiring_node_replays_ledger_and_marks_tick_unhealthy() {
    let mut ledger = super::ledger::NodeLedger::<u8>::default();
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
    let mut ledger = super::ledger::NodeLedger::<u8>::default();
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
    let scan = OneShotScan::new();

    assert!(
        scan.is_settled(
            &current,
            ScanPass {
                complete: true,
                healthy: true
            }
        ),
        "complete inventories keep the one-pass happy path"
    );
}

#[test]
fn one_shot_retry_waits_for_healthy_incomplete_inventory_to_stabilize() {
    let partial = inventory(&[1]);
    let full = inventory(&[1, 2]);
    let healthy = ScanPass {
        complete: false,
        healthy: true,
    };
    let mut scan = OneShotScan::new();

    assert!(
        !scan.is_settled(&partial, healthy),
        "the first incomplete pass has no previous inventory to compare"
    );
    scan.advance(partial, healthy);
    assert!(
        !scan.is_settled(&full, healthy),
        "a changed inventory should get another retry window"
    );
    scan.advance(full.clone(), healthy);
    assert!(
        scan.is_settled(&full, healthy),
        "once the returned inventory stabilizes, retrying stops"
    );
}

#[test]
fn one_shot_retry_stops_on_unchanged_incomplete_inventory() {
    let partial = inventory(&[1]);
    let healthy = ScanPass {
        complete: false,
        healthy: true,
    };
    let mut scan = OneShotScan::new();

    scan.advance(partial.clone(), healthy);
    assert!(
        scan.is_settled(&partial, healthy),
        "stable partial inventories should not burn every retry attempt"
    );
}

#[test]
fn one_shot_retry_keeps_unchanged_inventory_after_unhealthy_probe() {
    let partial = inventory(&[1]);
    let mut scan = OneShotScan::new();

    // The replayed snapshot arrived from an earlier healthy pass…
    scan.advance(
        partial.clone(),
        ScanPass {
            complete: false,
            healthy: true,
        },
    );
    // …but this pass failed, so the unchanged replay is not stability
    // evidence.
    assert!(
        !scan.is_settled(
            &partial,
            ScanPass {
                complete: false,
                healthy: false
            }
        ),
        "unchanged replay after a failed probe must keep retrying before the cap"
    );
}

#[test]
fn one_shot_retry_stops_at_attempt_cap_when_inventory_keeps_changing() {
    let unhealthy = ScanPass {
        complete: false,
        healthy: false,
    };
    let mut scan = OneShotScan::new();

    while scan.attempt < ONESHOT_ATTEMPTS {
        let changing = inventory(&[scan.attempt]);
        assert!(
            !scan.is_settled(&changing, unhealthy),
            "attempts below the cap keep retrying"
        );
        scan.advance(changing, unhealthy);
    }
    assert!(
        scan.is_settled(&inventory(&[1, 2]), unhealthy),
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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Healthy { complete: true },
        "a count matching the readable slots is authoritative and complete"
    );
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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Failed,
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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Failed,
        "no count register means we couldn't fully check"
    );
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

/// A control-table read that fails half way reads exactly like "no haptic
/// panel", and the answer is memoized for `REFRESH_INTERVAL` — so the Actions Ring
/// binding would vanish from the GUI for half a minute on a device that has it.
#[test]
fn an_incomplete_capability_walk_keeps_the_last_complete_answer() {
    let mut fresh = probed(None, false);
    fresh.capabilities_incomplete = true;
    fresh.capabilities = Some(Capabilities::default());
    let mut cached = probed(None, false);
    cached.capabilities = Some(Capabilities {
        haptic_panel: true,
        ..Capabilities::default()
    });

    keep_known_capabilities(&mut fresh, &cached);

    assert_eq!(
        fresh.capabilities.map(|caps| caps.haptic_panel),
        Some(true),
        "the panel the last complete walk saw must survive a lost reply"
    );
}

/// A device that genuinely lost a capability must still be able to say so.
#[test]
fn a_complete_capability_walk_is_left_alone() {
    let mut fresh = probed(None, false);
    fresh.capabilities = Some(Capabilities::default());
    let mut cached = probed(None, false);
    cached.capabilities = Some(Capabilities {
        haptic_panel: true,
        ..Capabilities::default()
    });

    keep_known_capabilities(&mut fresh, &cached);

    assert_eq!(
        fresh.capabilities.map(|caps| caps.haptic_panel),
        Some(false)
    );
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

/// A node the backend cannot open is a *failure*, not a disconnect: the tick
/// must report itself unhealthy so the one-shot retry runs its budget and the
/// ledger keeps replaying that node's last-good snapshot.
#[tokio::test]
async fn a_node_that_will_not_open_makes_the_tick_unhealthy() {
    let backend = ScriptedBackend::new(vec![(
        scripted_node_info("wont-open"),
        ScriptedNode::OpenFails,
    )]);
    let mut enumerator = Enumerator::with_backend(backend);

    let (inventories, complete, healthy) = enumerator
        .enumerate_reporting_completeness()
        .await
        .expect("enumeration itself must succeed — one node failing to open is not a fatal error");

    assert!(
        inventories.is_empty(),
        "a node that never opened has nothing to report"
    );
    assert!(
        !healthy,
        "a failed open must not be settled as a healthy probe"
    );
    assert!(!complete, "a failed open leaves the tick incomplete");
}

/// A node that opens but does not speak HID++ is simply not ours. It must not
/// be confused with a failed open: dragging the tick unhealthy for it would
/// make every host with an unrelated HID device retry forever.
#[tokio::test]
async fn a_non_hidpp_node_leaves_the_tick_healthy() {
    let backend = ScriptedBackend::new(vec![(
        scripted_node_info("not-hidpp"),
        ScriptedNode::NotHidpp,
    )]);
    let mut enumerator = Enumerator::with_backend(backend);

    let (inventories, complete, healthy) = enumerator
        .enumerate_reporting_completeness()
        .await
        .expect("enumeration must succeed");

    assert!(
        inventories.is_empty(),
        "a non-HID++ node contributes no inventory"
    );
    assert!(
        healthy,
        "a node that is not HID++ is not a failure to retry"
    );
    assert!(complete, "nothing was left unchecked");
}

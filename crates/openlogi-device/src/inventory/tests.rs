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
    NodeProbe, PassContext, ProbeVerdict, assemble_bolt_probe, assemble_unifying_device,
    parse_codename_unifying, preferred_direct_codename, probe_one, probe_unifying_slot,
    unifying_probe_budget,
};
use super::{
    ChannelCache, Enumerator, ONESHOT_ATTEMPTS, OneShotScan, ProbeDeadlines, ScanPass,
    UNIFYING_CACHED_SLOT_PROBE, UNIFYING_SLOT_PROBE, retained_nodes, routes_for_inventories,
    settle_probe, settle_unhealthy_node,
};
use crate::backend::{NodeId, NodeInfo};
use crate::channel::scripted::{
    ScriptedBackend, ScriptedNode, ScriptedRawHidChannel, scripted_channel, scripted_node_info,
};
use crate::host_lock;
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

/// A deferred tick is evidence of nothing about the node's devices either:
/// the entries the node contributed are held out of miss aging, however many
/// deferrals run back to back, while the entries of a node that was actually
/// checked — and did not report them — age as before. Once the deferred node
/// is probed again, normal aging resumes from where it stood.
#[test]
fn deferred_ticks_hold_the_nodes_cache_entries_out_of_miss_aging() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let deferred_node = NodeId::from("deferred-receiver".to_string());
    let checked_node = NodeId::from("checked-receiver".to_string());
    let held = CacheKey::Bolt { unit_id: [1; 4] };
    let aged = CacheKey::Bolt { unit_id: [2; 4] };
    let answered = |outcomes| NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes,
    };
    // One pass's cache bookkeeping for a set of settled probes: the shape of
    // `enumerate_reporting_completeness`, without the channels.
    let pass = |e: &mut Enumerator, probes: Vec<(&NodeId, NodeProbe)>| {
        let mut frozen = HashSet::new();
        let mut outcomes = Vec::new();
        for (node, probe) in probes {
            settle_probe(&mut e.ledger, node, probe.verdict, probe.inventory.clone());
            e.hold_or_note_cache_keys(node, &probe, &mut frozen);
            outcomes.extend(probe.outcomes);
        }
        let seen = e.apply_outcomes(outcomes);
        e.evict_unseen(&seen, &frozen);
    };

    // Both nodes answer and contribute an entry each.
    pass(
        &mut e,
        vec![
            (
                &deferred_node,
                answered(vec![CacheOutcome::Fresh(held.clone(), cache_entry())]),
            ),
            (
                &checked_node,
                answered(vec![CacheOutcome::Fresh(aged.clone(), cache_entry())]),
            ),
        ],
    );

    // Then one pass past the grace in which the first node's probe is
    // deferred and the second answers without its device.
    for _ in 0..=CACHE_MISS_GRACE {
        pass(
            &mut e,
            vec![
                (&deferred_node, NodeProbe::deferred()),
                (&checked_node, answered(Vec::new())),
            ],
        );
    }
    assert!(
        e.cache.contains_key(&held),
        "a deferred node's entry must not age out"
    );
    assert!(
        !e.cache.contains_key(&aged),
        "a checked node's unreported entry ages as before"
    );
    assert!(
        !e.node_cache_keys[&checked_node].contains(&aged),
        "an evicted entry is no longer the node's to hold"
    );

    // The deferred node is probed again and does not report its device:
    // aging resumes.
    for _ in 0..=CACHE_MISS_GRACE {
        pass(&mut e, vec![(&deferred_node, answered(Vec::new()))]);
    }
    assert!(
        !e.cache.contains_key(&held),
        "normal aging resumes once the node is actually checked"
    );
}

/// A warm start loads persisted entries before any receiver has answered,
/// so a receiver deferred from its very first probe has no record of what is
/// its. Every unattributed entry is held for it until it is actually probed;
/// an entry another node has claimed is not.
#[test]
fn a_node_deferred_before_its_first_probe_holds_every_unattributed_entry() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let receiver = NodeId::from("warm-receiver".to_string());
    let checked_node = NodeId::from("checked-receiver".to_string());
    let persisted = CacheKey::Bolt { unit_id: [1; 4] };
    let claimed = CacheKey::Bolt { unit_id: [2; 4] };
    // The persisted entry was loaded, never attributed; another node
    // contributed — and now stops reporting — an entry of its own.
    e.cache.insert(persisted.clone(), cache_entry());
    let mut frozen = HashSet::new();
    let claim = NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: vec![CacheOutcome::Fresh(claimed.clone(), cache_entry())],
    };
    e.hold_or_note_cache_keys(&checked_node, &claim, &mut frozen);
    e.apply_outcomes(claim.outcomes);

    for _ in 0..=CACHE_MISS_GRACE {
        let mut frozen = HashSet::new();
        let deferred = NodeProbe::deferred();
        settle_probe(&mut e.ledger, &receiver, deferred.verdict, None);
        e.hold_or_note_cache_keys(&receiver, &deferred, &mut frozen);
        let checked = NodeProbe {
            inventory: None,
            verdict: ProbeVerdict::Healthy { complete: true },
            outcomes: Vec::new(),
        };
        e.hold_or_note_cache_keys(&checked_node, &checked, &mut frozen);
        assert_eq!(
            frozen,
            HashSet::from([persisted.clone()]),
            "only the unattributed entry is held for the never-probed node"
        );
        let seen = e.apply_outcomes(deferred.outcomes);
        e.evict_unseen(&seen, &frozen);
    }
    assert!(
        e.cache.contains_key(&persisted),
        "a persisted entry must survive deferrals of the receiver that has yet to claim it"
    );
    assert!(
        !e.cache.contains_key(&claimed),
        "the checked node's unreported entry ages as before"
    );

    // The receiver's first real probe claims nothing: from then on its
    // deferrals hold nothing, and the persisted entry ages normally.
    let first = NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: Vec::new(),
    };
    e.hold_or_note_cache_keys(&receiver, &first, &mut HashSet::new());
    for _ in 0..=CACHE_MISS_GRACE {
        let mut frozen = HashSet::new();
        e.hold_or_note_cache_keys(&receiver, &NodeProbe::deferred(), &mut frozen);
        assert!(
            frozen.is_empty(),
            "a probed node holds only what it claimed"
        );
        e.evict_unseen(&HashSet::new(), &frozen);
    }
    assert!(!e.cache.contains_key(&persisted));
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
    let deadlines = &ProbeDeadlines::DEFAULT;
    assert_eq!(
        unifying_probe_budget(Some(&cached), cached.probed_at, deadlines),
        UNIFYING_CACHED_SLOT_PROBE
    );
    assert_eq!(
        unifying_probe_budget(
            Some(&cached),
            cached.probed_at + REFRESH_INTERVAL,
            deadlines
        ),
        UNIFYING_SLOT_PROBE,
        "stale entries still get enough time for a full feature walk"
    );
    assert_eq!(
        unifying_probe_budget(None, Instant::now(), deadlines),
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
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        deadlines: &ProbeDeadlines::DEFAULT,
    };
    let (device, _) = probe_unifying_slot(&channel, &event, "SERIAL", pass)
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

/// The Logi Bolt receiver's product id.
const BOLT_RECEIVER_PID: u16 = 0xc548;

/// The unit id [`bolt_receiver_with_a_silent_slot`] reports for slot 1.
const SILENT_SLOT_UNIT_ID: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

/// A Bolt receiver with one paired mouse in slot 1. The receiver answers
/// every register read at once — no arrival events, so the drain runs to its
/// deadline and the slot is read from the pairing register — while the mouse
/// itself, addressed at its slot index, never answers: a slot whose feature
/// walk runs to its own budget and falls back to the cache.
fn bolt_receiver_with_a_silent_slot(request: &[u8]) -> Option<Vec<u8>> {
    let [_, device, sub_id, address, sub_register, ..] = *request else {
        return None;
    };
    if device != 0xff {
        return None;
    }
    let short = |data: [u8; 3]| Some(vec![0x10, 0xff, sub_id, address, data[0], data[1], data[2]]);
    let long = |data: &[u8]| {
        let mut report = vec![0x11, 0xff, 0x83, address];
        report.extend_from_slice(data);
        report.resize(20, 0);
        Some(report)
    };
    match (sub_id, address) {
        // Notifications (wireless notifications already on) and Connections
        // (one pairing) happen to read the same.
        (0x81, 0x00 | 0x02) => short([0x00, 0x01, 0x00]),
        // Register writes (the arrival trigger) are acknowledged.
        (0x80, _) => short([0x00, 0x00, 0x00]),
        // Unique id: sixteen ASCII bytes.
        (0x83, 0xfb) => long(b"0000000012345678"),
        (0x83, 0xb5) => match sub_register {
            // Slot 1's pairing information: a mouse, online, wpid c09d.
            0x51 => long(&[0x51, 0x02, 0x9d, 0xc0, 0xde, 0xad, 0xbe, 0xef]),
            // Slot 1's codename.
            0x61 => long(&[
                0x61, 0x01, 0x08, b'M', b'X', b' ', b'P', b'r', b'o', b'b', b'e',
            ]),
            // Every other slot is empty: an error reply, no sub-register byte.
            _ => Some(vec![0x10, 0xff, 0x8f, 0x83, 0xb5, 0x08, 0x00]),
        },
        _ => None,
    }
}

/// A Bolt receiver node whose register phase no other test shares.
fn bolt_receiver_node(tag: &str) -> NodeInfo {
    let mut info = scripted_node_info(&format!("{tag}-{}", std::process::id()));
    info.product_id = BOLT_RECEIVER_PID;
    info
}

/// Deadlines shrunk to test scale, in the production proportions: the slot
/// probe and the drain fit the receiver budget with room, and the register
/// lock wait is long enough for a holder to release inside it.
fn quick_deadlines() -> ProbeDeadlines {
    ProbeDeadlines {
        register_lock_wait: Duration::from_secs(2),
        receiver_budget: Duration::from_millis(900),
        direct_budget: Duration::from_millis(900),
        arrival_drain: Duration::from_millis(100),
        bolt_slot_probe: Duration::from_millis(400),
        unifying_slot_probe: Duration::from_millis(400),
        unifying_cached_slot_probe: Duration::from_millis(100),
    }
}

/// A stale cache entry for the silent slot, so its walk runs and has
/// something to fall back to when it times out.
fn stale_silent_slot_cache() -> (CacheKey, HashMap<CacheKey, Cached>) {
    let key = CacheKey::Bolt {
        unit_id: SILENT_SLOT_UNIT_ID,
    };
    let mut entry = cache_entry();
    entry.probe = probed(Some(model(SILENT_SLOT_UNIT_ID, Some("SN-1"))), false);
    entry.probed_at = Instant::now()
        .checked_sub(REFRESH_INTERVAL)
        .expect("the process has been up longer than the refresh interval's worth of ticks");
    let cache = HashMap::from([(key.clone(), entry)]);
    (key, cache)
}

/// The register-phase wait sits outside the receiver's I/O budget: a probe
/// that waited for another process to release the phase — inside its wait
/// budget — still gets the whole I/O budget the receiver's worst case was
/// sized for, so a slow slot reaches its normal timeout-and-cache fallback
/// and the receiver settles healthy. Composed with the wait plus the I/O
/// exceeding the budget on purpose: under one deadline around both, this
/// probe failed — and two such failures retire a working receiver's channel.
#[tokio::test]
async fn a_receiver_probe_that_waited_for_its_register_phase_keeps_its_whole_io_budget() {
    let info = bolt_receiver_node("register-phase-wait");
    let deadlines = quick_deadlines();
    let lock_held_for = Duration::from_millis(600);
    // Another process (here: this test) holds the receiver's register phase,
    // releasing it inside the wait but late enough that wait + I/O outruns
    // the receiver budget.
    let held = host_lock::try_lock(&host_lock::node_lock_name(&info.id))
        .unwrap()
        .expect("the test takes the phase first");
    let release = tokio::spawn(async move {
        tokio::time::sleep(lock_held_for).await;
        drop(held);
    });
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let (key, cache) = stale_silent_slot_cache();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        deadlines: &deadlines,
    };

    let started = Instant::now();
    let probe = probe_one(info, channel, pass).await;
    release.await.unwrap();

    let io_floor = deadlines.arrival_drain + deadlines.bolt_slot_probe;
    assert!(
        lock_held_for + io_floor > deadlines.receiver_budget,
        "the test must compose a wait and an I/O floor that together outrun the budget"
    );
    assert!(
        started.elapsed() >= lock_held_for + io_floor,
        "the probe waited for the phase and the slot ran to its own budget: {:?}",
        started.elapsed()
    );
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Healthy { complete: true },
        "the wait must not have eaten into the I/O budget"
    );
    let inventory = probe.inventory.expect("the receiver answered");
    assert_eq!(
        inventory.receiver.unique_id.as_deref(),
        Some("0000000012345678")
    );
    assert_eq!(inventory.paired.len(), 1);
    let device = &inventory.paired[0];
    assert_eq!(device.slot, 1);
    assert_eq!(device.codename.as_deref(), Some("MX Probe"));
    assert!(device.online);
    assert_eq!(
        device.model_info.as_ref().map(|m| m.unit_id),
        Some(SILENT_SLOT_UNIT_ID),
        "the timed-out slot fell back to its cached probe"
    );
    assert!(
        matches!(probe.outcomes.as_slice(), [CacheOutcome::Seen(seen)] if *seen == key),
        "a timed-out slot keeps its cache entry alive without refreshing it"
    );
}

/// A receiver whose register phase stays held past the wait is deferred
/// without a byte of I/O: nothing was checked, so nothing is reported as
/// failed.
#[tokio::test]
async fn a_receiver_probe_defers_when_the_register_phase_is_held_past_the_wait() {
    let info = bolt_receiver_node("register-phase-held");
    let deadlines = ProbeDeadlines {
        register_lock_wait: Duration::from_millis(100),
        ..quick_deadlines()
    };
    let _held = host_lock::try_lock(&host_lock::node_lock_name(&info.id))
        .unwrap()
        .expect("the test takes the phase first");
    let (raw, handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        deadlines: &deadlines,
    };

    let probe = probe_one(info, channel, pass).await;

    assert_eq!(probe.verdict, ProbeVerdict::Deferred);
    assert!(probe.inventory.is_none());
    assert!(probe.outcomes.is_empty());
    assert!(
        handle.written_reports().is_empty(),
        "a deferred probe must not touch the receiver"
    );
}

/// The I/O budget still bounds the probe on its own: a receiver whose slot
/// walk alone outruns it is a failed probe, for the ledger to replay through.
#[tokio::test]
async fn a_receiver_probe_whose_io_outruns_the_budget_is_failed() {
    let info = bolt_receiver_node("io-outruns-budget");
    let deadlines = ProbeDeadlines {
        receiver_budget: Duration::from_millis(150),
        ..quick_deadlines()
    };
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let (_, cache) = stale_silent_slot_cache();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        deadlines: &deadlines,
    };

    let probe = probe_one(info, channel, pass).await;

    assert_eq!(probe.verdict, ProbeVerdict::Failed);
    assert!(probe.inventory.is_none());
}

//! What a settled tick publishes: exact routes, retired channels, ledger replay and tick health.

use super::*;

#[test]
fn offline_slots_remain_in_inventory_without_a_live_route() {
    let mut receiver = inventory(&[1, 4]);
    receiver[0].paired[0].online = false;
    assert_eq!(
        routes_for_inventories(&receiver),
        vec![DeviceRoute::Unifying {
            receiver_uid: "receiver-1".into(),
            slot: 4,
        }],
        "an inactive device route must not survive merely because its USB receiver remains connected"
    );
    receiver[0].paired[1].online = false;
    assert!(routes_for_inventories(&receiver).is_empty());
    assert_eq!(
        receiver[0].paired.len(),
        2,
        "remembered pairings remain intact"
    );
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
            settings_revision: 0,
            hidpp10: false,
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

type NodeChannels = ChannelCache<u8, Arc<()>>;

/// Drives node 1 to the pass that may open it.
type History = fn(&mut NodeChannels);

fn quiescent(channel: &Arc<()>) -> bool {
    Arc::strong_count(channel) == 1
}

/// Node 1's share of one `prepare_nodes` pass, in its order, with node 1
/// listed or not. Returns whether the pass would open the node.
fn channel_pass(cache: &mut NodeChannels, listed: bool) -> bool {
    cache.begin_pass();
    let seen = if listed {
        HashSet::from([1])
    } else {
        HashSet::new()
    };
    let opens = listed && cache.prepare_open(&1, quiescent) && cache.get(&1).is_none();
    cache.retire_absent(&seen);
    cache.reap_absent(&seen, quiescent);
    opens
}

/// Node 1's channel fails and drains while the OS keeps listing the node.
fn evict_and_drain(cache: &mut NodeChannels) {
    cache.open(1, Arc::new(()), || false);
    assert!(cache.evict(&1), "a serving channel retires");
    assert!(
        !channel_pass(cache, true),
        "the pass that drops the drained retirement still skips opening"
    );
}

#[test]
fn channel_cache_retires_and_defers_reopen_until_a_later_tick() {
    let mut cache = NodeChannels::default();
    let channel = Arc::new(());
    cache.open(1, Arc::clone(&channel), || false);

    assert!(cache.evict(&1));
    assert!(cache.get(&1).is_none());
    assert!(!cache.prepare_open(&1, quiescent));

    drop(channel);
    assert!(cache.is_retiring(&1));
    assert!(
        !cache.prepare_open(&1, quiescent),
        "the tick that drops retirement still skips opening"
    );
    assert!(!cache.is_retiring(&1));
    assert!(
        cache.prepare_open(&1, quiescent),
        "only a later tick may reopen"
    );
}

#[test]
fn absent_channels_retire_and_quiescent_absent_retirement_is_reaped() {
    let mut cache = NodeChannels::default();
    cache.open(1, Arc::new(()), || false);
    cache.open(2, Arc::new(()), || false);

    let retired = cache.retire_absent(&HashSet::from([2]));
    assert_eq!(retired, 1, "one absent channel retires once");
    assert!(cache.is_retiring(&1));
    assert!(cache.get(&2).is_some());

    cache.reap_absent(&HashSet::from([2]), quiescent);
    assert!(!cache.is_retiring(&1));
}

/// Every way a node reaches its next open, against whether the ledger still
/// replays it online: only a failed channel that drained while the OS kept
/// listing its node, and is still replayed online, comes back as the
/// replacement no snapshot showed — and reports it for that one pass.
#[test]
fn only_a_drained_failed_channel_still_replayed_online_reopens_as_a_replacement() {
    let histories: [(&str, History, bool); 5] = [
        ("never opened", |_| {}, false),
        ("failed and drained while listed", evict_and_drain, true),
        (
            "failed, then left the listing while draining",
            |cache| {
                let holder = Arc::new(());
                cache.open(1, Arc::clone(&holder), || false);
                assert!(cache.evict(&1));
                assert!(!channel_pass(cache, false));
                drop(holder);
                assert!(!channel_pass(cache, true));
            },
            false,
        ),
        (
            "failed and drained, then left the listing",
            |cache| {
                evict_and_drain(cache);
                assert!(!channel_pass(cache, false));
            },
            false,
        ),
        (
            "left the listing",
            |cache| {
                cache.open(1, Arc::new(()), || false);
                assert!(!channel_pass(cache, false));
            },
            false,
        ),
    ];
    for (history, reach, awaits_replacement) in histories {
        for replayed_online in [false, true] {
            let mut cache = NodeChannels::default();
            reach(&mut cache);
            assert!(channel_pass(&mut cache, true), "{history}: the node opens");
            cache.open(1, Arc::new(()), || replayed_online);

            assert_eq!(
                cache.has_replacement(),
                awaits_replacement && replayed_online,
                "{history}, replayed online: {replayed_online}"
            );
            assert!(cache.get(&1).is_some(), "{history}: the reopen serves");
            assert!(!channel_pass(&mut cache, true), "{history}: no second open");
            assert!(
                !cache.has_replacement(),
                "{history}: the next pass serves the replacement as an ordinary channel"
            );
            assert!(cache.get(&1).is_some());
        }
    }
}

/// A replacement whose own pass publishes nothing for the node surfaces it
/// offline, so the agent observes the return; the channel keeps serving.
#[test]
fn a_replacement_surfaced_offline_is_an_ordinary_reconnect() {
    let mut cache = NodeChannels::default();
    evict_and_drain(&mut cache);
    assert!(channel_pass(&mut cache, true));
    cache.open(1, Arc::new(()), || true);
    assert!(cache.has_replacement());

    cache.surfaced_offline(&1);
    assert!(!cache.has_replacement());
    assert!(cache.get(&1).is_some(), "the replacement keeps serving");
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

/// A node the backend cannot open is a *failure*, not a disconnect: the tick
/// must report itself unhealthy so the one-shot retry runs its budget and the
/// ledger keeps replaying that node's last-good snapshot.
#[tokio::test]
async fn a_node_that_will_not_open_makes_the_tick_unhealthy() {
    let backend =
        ScriptedBackend::new(vec![(scripted_node_info("wont-open"), ScriptedOpen::Fails)]);
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
        ScriptedOpen::NotHidpp,
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

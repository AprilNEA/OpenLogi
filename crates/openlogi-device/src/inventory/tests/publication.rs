//! What a settled tick publishes: exact routes, retired channels, ledger replay and tick health.

use super::*;

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

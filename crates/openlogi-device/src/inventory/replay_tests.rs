use std::sync::Arc;
use std::time::{Duration, Instant};

use openlogi_core::device::{BatteryInfo, BatteryStatus, LockKey};
use openlogi_fixture::RequestMatch;
use tokio::sync::mpsc;

use super::Enumerator;
use super::cache::{CACHE_MISS_GRACE, CacheKey, REFRESH_INTERVAL};
use super::events::{HidppEventSource, event_channel, observed_event_channel};
use super::replay_test_support::{
    BOLT_CHANNEL, BOLT_UID, BoltSlot, DIRECT_CHANNEL, HEADSET_CHANNEL, HEADSET_PRODUCT_ID,
    adc_error, adc_reading, battery_only_fixture, bolt_fixture, connection_notification,
    direct_fixture, headset_fixture, lock_key_fixture, malformed_dpi_fixture, setting_fixture,
    short, voltage_reading,
};
use crate::replay::{ChannelConnection, NodePresence, OpenOutcome, ReplayBackend, ReplayTopology};
use crate::{ChannelRegistry, get_dpi};

#[tokio::test]
async fn receiver_slots_interleave_on_one_channel_and_lifecycle_events_coalesce() {
    let slots = [
        BoltSlot {
            slot: 1,
            online: true,
        },
        BoltSlot {
            slot: 2,
            online: true,
        },
        BoltSlot {
            slot: 3,
            online: false,
        },
    ];
    let fixture = bolt_fixture("interleaved-slots", &slots, 1);
    let expected = fixture.inventory.clone();
    let node_id = fixture.node_id.clone();
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid multi-slot replay"),
    );
    let slot_one = backend
        .hold_next_response(
            BOLT_CHANNEL,
            RequestMatch::Hidpp20,
            &short(1, 0, 0x10, [0, 0, 0]),
        )
        .expect("known channel");
    let slot_two = backend
        .hold_next_response(
            BOLT_CHANNEL,
            RequestMatch::Hidpp20,
            &short(2, 0, 0x10, [0, 0, 0]),
        )
        .expect("known channel");
    let (notifier, mut events, observed) = observed_event_channel();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_event_notifier(notifier);
    enumerator.timeouts.arrival_drain = Duration::ZERO;

    let (inventory, ()) = tokio::join!(enumerator.enumerate(), async {
        tokio::join!(slot_one.request_written(), slot_two.request_written());
        slot_two.release();
        slot_one.release();
    });
    let inventory = inventory.expect("receiver probe succeeds");

    assert_eq!(inventory, [expected]);
    assert_eq!(backend.open_count(&node_id).expect("known node"), 1);
    assert_eq!(
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
            .channel_open_count,
        1
    );
    assert!(
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
            .written_reports
            .iter()
            .all(|report| report.get(1) != Some(&3)),
        "the sleeping slot must surface from pairing registers without feature probes"
    );
    backend
        .require_complete()
        .expect("all interleaved receiver exchanges consumed");

    let notification = connection_notification(1);
    assert_eq!(
        backend
            .emit_channel_report(BOLT_CHANNEL, &notification)
            .expect("known channel"),
        1
    );
    assert_eq!(
        backend
            .emit_channel_report(BOLT_CHANNEL, &notification)
            .expect("known channel"),
        1
    );
    observed.wait_for(2).await;
    assert_eq!(events.try_recv(), Ok(HidppEventSource::ReceiverConnection));
    assert_eq!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

#[tokio::test]
async fn transient_open_failure_requests_one_bounded_repair() {
    let fixture = direct_fixture(OpenOutcome::Denied, 1);
    let expected = fixture.inventory.clone();
    let node_id = fixture.node_id.clone();
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid direct replay"),
    );
    let mut enumerator = Enumerator::with_backend(backend.clone());

    let (failed, complete, healthy) = enumerator
        .enumerate_reporting_completeness()
        .await
        .expect("a per-node open error is not a fatal enumeration error");
    assert!(failed.is_empty());
    assert!(!complete);
    assert!(!healthy);
    assert!(enumerator.retry_needed_last_tick());
    assert_eq!(backend.open_count(&node_id).expect("known node"), 1);

    backend
        .set_open_outcome(&node_id, OpenOutcome::Hidpp)
        .expect("known node");
    let (repaired, complete, healthy) = enumerator
        .enumerate_reporting_completeness()
        .await
        .expect("repair succeeds");

    assert_eq!(repaired, [expected]);
    assert!(complete);
    assert!(healthy);
    assert!(!enumerator.retry_needed_last_tick());
    assert_eq!(backend.open_count(&node_id).expect("known node"), 2);
    assert_eq!(
        backend
            .channel_completion(DIRECT_CHANNEL)
            .expect("known channel")
            .channel_open_count,
        1,
        "the denied attempt must not create a raw channel lifetime"
    );
    backend.require_complete().expect("repair probe consumed");
}

#[tokio::test]
async fn disconnected_stale_channel_replays_last_good_then_opens_a_replacement() {
    let fixture = bolt_fixture(
        "disconnected-channel",
        &[BoltSlot {
            slot: 1,
            online: true,
        }],
        2,
    );
    let expected = fixture.inventory.clone();
    let route = crate::DeviceRoute::Bolt {
        receiver_uid: BOLT_UID.to_string(),
        slot: 1,
    };
    let node_id = fixture.node_id.clone();
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid Bolt replay"),
    );
    let registry = ChannelRegistry::default();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_registry(registry.clone());
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    let initial = enumerator
        .enumerate()
        .await
        .expect("initial probe succeeds");
    assert_eq!(
        initial,
        std::slice::from_ref(&expected),
        "receiver completion: {:#?}",
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
    );
    let stale = registry.lookup(&route).expect("receiver route published");

    backend
        .set_channel_connection(BOLT_CHANNEL, ChannelConnection::Disconnected)
        .expect("known channel");
    let first_failure = enumerator.enumerate().await.expect("failed probe replays");
    assert_eq!(first_failure, std::slice::from_ref(&expected));
    assert!(enumerator.ledger.contains_last_good(&node_id));
    assert!(!stale.channel().is_connected());
    assert!(registry.is_current(&stale));

    let second_failure = enumerator
        .enumerate()
        .await
        .expect("second failed probe retires");
    assert_eq!(second_failure, std::slice::from_ref(&expected));
    assert!(enumerator.channels.is_retiring(&node_id));
    assert!(!registry.is_current(&stale));
    assert!(registry.lookup(&route).is_none());
    drop(stale);

    backend
        .set_channel_connection(BOLT_CHANNEL, ChannelConnection::Connected)
        .expect("known channel");
    let draining = enumerator
        .enumerate()
        .await
        .expect("quiescent retirement is deferred for one pass");
    assert_eq!(draining, std::slice::from_ref(&expected));
    let repaired = enumerator
        .enumerate()
        .await
        .expect("replacement probe succeeds");

    assert_eq!(repaired, [expected]);
    let replacement = registry.lookup(&route).expect("replacement published");
    assert!(replacement.channel().is_connected());
    assert!(registry.is_current(&replacement));
    assert_eq!(backend.open_count(&node_id).expect("known node"), 2);
    let completion = backend
        .channel_completion(BOLT_CHANNEL)
        .expect("known channel");
    assert_eq!(completion.channel_open_count, 2);
    assert_eq!(
        backend
            .channel_lifetime_count(BOLT_CHANNEL)
            .expect("known channel"),
        1,
        "the disconnected lifetime must be retired before replacement"
    );
    completion
        .require_complete()
        .expect("both successful probe cassettes consumed");
}

#[tokio::test]
async fn vanished_direct_node_ages_out_independently_of_a_sleeping_receiver_slot() {
    let direct = direct_fixture(OpenOutcome::Hidpp, 1);
    let sleeping = bolt_fixture(
        "sleeping-slot",
        &[BoltSlot {
            slot: 1,
            online: false,
        }],
        6,
    );
    let direct_inventory = direct.inventory.clone();
    let sleeping_inventory = sleeping.inventory.clone();
    let direct_id = direct.node_id.clone();
    let sleeping_id = sleeping.node_id.clone();
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![direct.node, sleeping.node],
                channels: vec![direct.channel, sleeping.channel],
            },
            vec![direct.cassette, sleeping.cassette],
        )
        .expect("valid mixed replay"),
    );
    let mut enumerator = Enumerator::with_backend(backend.clone());
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    let initial = enumerator.enumerate().await.expect("mixed probe succeeds");
    assert!(initial.contains(&direct_inventory));
    assert!(
        initial.contains(&sleeping_inventory),
        "actual inventory: {initial:#?}; receiver completion: {:#?}",
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
    );
    let direct_key = CacheKey::Direct(direct_id.clone());
    assert!(enumerator.cache.contains_key(&direct_key));

    backend
        .set_node_presence(&direct_id, NodePresence::Absent)
        .expect("known node");
    backend
        .set_channel_connection(DIRECT_CHANNEL, ChannelConnection::Disconnected)
        .expect("known channel");
    for miss in 1..=CACHE_MISS_GRACE {
        let inventory = enumerator
            .enumerate()
            .await
            .expect("sleeping receiver remains enumerable");
        assert_eq!(inventory, std::slice::from_ref(&sleeping_inventory));
        assert!(
            enumerator.cache.contains_key(&direct_key),
            "direct cache retired inside grace on miss {miss}"
        );
    }
    let after_grace = enumerator.enumerate().await.expect("cache grace advances");
    assert_eq!(after_grace, std::slice::from_ref(&sleeping_inventory));
    assert!(!enumerator.cache.contains_key(&direct_key));
    assert!(!enumerator.ledger.contains_last_good(&direct_id));
    assert!(enumerator.ledger.contains_last_good(&sleeping_id));
    assert_eq!(
        backend
            .channel_lifetime_count(DIRECT_CHANNEL)
            .expect("known channel"),
        0
    );

    backend
        .set_node_presence(&direct_id, NodePresence::Present)
        .expect("known node");
    backend
        .set_channel_connection(DIRECT_CHANNEL, ChannelConnection::Connected)
        .expect("known channel");
    backend
        .set_open_outcome(&direct_id, OpenOutcome::Denied)
        .expect("known node");
    let reappeared_but_denied = enumerator
        .enumerate()
        .await
        .expect("per-node reopen failure is bounded");
    assert_eq!(reappeared_but_denied, [sleeping_inventory]);
    assert_eq!(backend.open_count(&direct_id).expect("known node"), 2);
    assert_eq!(backend.open_count(&sleeping_id).expect("known node"), 1);
    backend
        .require_complete()
        .expect("receiver passes and initial direct probe consumed");
}

#[tokio::test]
async fn malformed_response_is_released_only_after_the_request_barrier() {
    let fixture = malformed_dpi_fixture();
    let node_id = fixture.node_id.clone();
    let route = fixture.route.clone();
    let held_request = fixture.held_request.clone();
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid malformed-response replay"),
    );
    let barrier = backend
        .hold_next_response(BOLT_CHANNEL, RequestMatch::Hidpp20, &held_request)
        .expect("known channel");
    let operation = tokio::spawn({
        let backend = Arc::clone(&backend);
        async move { get_dpi(&*backend, &route).await }
    });

    barrier.request_written().await;
    assert!(barrier.is_request_written());
    assert!(
        !operation.is_finished(),
        "the production DPI read must still await the held response"
    );
    assert!(
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
            .is_complete(),
        "the written request consumes the cassette before response release"
    );
    barrier.release();

    operation
        .await
        .expect("DPI task joins")
        .expect_err("the released response carries a malformed error code");
    assert_eq!(backend.open_count(&node_id).expect("known node"), 1);
    assert_eq!(
        backend
            .channel_completion(BOLT_CHANNEL)
            .expect("known channel")
            .channel_open_count,
        1
    );
    backend
        .require_complete()
        .expect("malformed response cassette consumed");
}

/// Enumerate once and return the `(online, battery reading)` of the node
/// listed under `product_id`.
async fn battery_node_reading(
    enumerator: &mut Enumerator,
    product_id: u16,
) -> (bool, Option<BatteryInfo>) {
    let inventories = enumerator.enumerate().await.expect("node probe succeeds");
    let node = inventories
        .iter()
        .find(|inventory| inventory.receiver.product_id == product_id)
        .unwrap_or_else(|| panic!("node {product_id:#06x} listed: {inventories:#?}"));
    let device = &node.paired[0];
    (device.online, device.battery.clone())
}

/// Enumerate once and return the `(online, battery percentage)` of the node
/// listed under `product_id`.
async fn battery_node_state(enumerator: &mut Enumerator, product_id: u16) -> (bool, Option<u8>) {
    let (online, battery) = battery_node_reading(enumerator, product_id).await;
    (online, battery.and_then(|battery| battery.percentage))
}

/// Enumerate once and return the headset's `(online, battery percentage)`.
async fn headset_state(enumerator: &mut Enumerator) -> (bool, Option<u8>) {
    battery_node_state(enumerator, HEADSET_PRODUCT_ID).await
}

#[tokio::test]
async fn headset_dongle_goes_offline_while_its_headset_is_off() {
    // On, switched off (the G733 dongle's captured 0x05 answer), on again.
    let fixture = headset_fixture(&[
        adc_reading(3890, 0x01),
        adc_error(0x05),
        adc_reading(3885, 0x01),
    ]);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid headset replay"),
    );
    let mut enumerator = Enumerator::with_backend(backend.clone());
    enumerator.timeouts.arrival_drain = Duration::ZERO;

    assert_eq!(headset_state(&mut enumerator).await, (true, Some(65)));
    assert_eq!(
        headset_state(&mut enumerator).await,
        (false, None),
        "an unlinked headset is offline and its last reading is cleared"
    );
    assert_eq!(headset_state(&mut enumerator).await, (true, Some(64)));
    backend
        .require_complete()
        .expect("every scripted headset exchange consumed");
}

#[tokio::test]
async fn headset_stale_reprobe_preserves_unavailable_link_state() {
    let offline = (false, None);
    let online = (true, Some(65));
    let cases = [
        (adc_error(0x05), offline, adc_error(0x08), offline),
        (adc_error(0x05), offline, adc_reading(3905, 0x0f), offline),
        (adc_reading(3890, 0x01), online, adc_error(0x08), online),
        (
            adc_reading(3890, 0x01),
            online,
            adc_reading(3905, 0x0f),
            online,
        ),
        (adc_reading(3890, 0x01), online, adc_error(0x05), offline),
        (adc_error(0x05), offline, adc_reading(3890, 0x01), online),
    ];
    for (initial, before, reprobed, after) in cases {
        let mut fixture = headset_fixture(&[initial]);
        let key = CacheKey::Direct(fixture.node.info.id.clone());
        // Repeat the immutable walk before the next ADC answer; recovery then uses the cache.
        fixture.cassette.exchanges.extend(
            headset_fixture(&[reprobed, adc_reading(3885, 0x01)])
                .cassette
                .exchanges,
        );
        let backend = Arc::new(
            ReplayBackend::new(
                ReplayTopology {
                    nodes: vec![fixture.node],
                    channels: vec![fixture.channel],
                },
                vec![fixture.cassette],
            )
            .expect("valid headset replay"),
        );
        let mut enumerator = Enumerator::with_backend(backend.clone());
        enumerator.timeouts.arrival_drain = Duration::ZERO;

        assert_eq!(headset_state(&mut enumerator).await, before);
        enumerator
            .cache
            .get_mut(&key)
            .expect("the initial probe is cached")
            .probed_at = Instant::now()
            .checked_sub(REFRESH_INTERVAL + Duration::from_nanos(1))
            .expect("the refresh interval fits the monotonic clock");
        assert_eq!(
            headset_state(&mut enumerator).await,
            after,
            "a full reprobe changes link state only when ADC supplies evidence"
        );
        assert_eq!(headset_state(&mut enumerator).await, (true, Some(64)));
        backend
            .require_complete()
            .expect("the full reprobe and recovery exchanges are consumed");
    }
}

#[tokio::test]
async fn headset_first_seen_while_off_is_listed_offline() {
    // No lighting or pointer feature: only the unlinked answer proves a
    // device, so the node must not be rejected as a receiver's side interface.
    let fixture = headset_fixture(&[adc_error(0x05)]);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid headset replay"),
    );
    let mut enumerator = Enumerator::with_backend(backend.clone());
    enumerator.timeouts.arrival_drain = Duration::ZERO;

    assert_eq!(headset_state(&mut enumerator).await, (false, None));
    backend
        .require_complete()
        .expect("every scripted headset exchange consumed");
}

#[tokio::test]
async fn headset_switch_off_broadcast_requests_reconciliation_to_offline() {
    let fixture = headset_fixture(&[adc_reading(3890, 0x01), adc_error(0x05)]);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid headset replay"),
    );
    let (notifier, mut events, observed) = observed_event_channel();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_event_notifier(notifier);
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    assert_eq!(headset_state(&mut enumerator).await, (true, Some(65)));

    // The broadcast a G733 dongle sent as its headset switched off (captured
    // as `11 ff 08 00 00 00 00`; 0x1F20 sits at index 2 in this fixture).
    let mut switched_off = vec![0x11, 0xff, 0x02, 0x00];
    switched_off.extend_from_slice(&[0; 16]);
    assert_eq!(
        backend
            .emit_channel_report(HEADSET_CHANNEL, &switched_off)
            .expect("known channel"),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), observed.wait_for(1))
        .await
        .expect("the broadcast must be recognised as a reconciliation request");
    assert_eq!(events.try_recv(), Ok(HidppEventSource::AdcMeasurement));

    // The reconciliation it requests re-reads the battery and finds the
    // headset unlinked, without waiting for the recovery scan.
    assert_eq!(headset_state(&mut enumerator).await, (false, None));
    backend
        .require_complete()
        .expect("every scripted headset exchange consumed");
}

/// Probe a node whose only battery source is `feature_id` (at index 2), emit
/// that feature's event-`0` broadcast carrying `broadcast`, and return the
/// battery percentage before and after the reconciliation it requests.
async fn battery_broadcast_refresh(
    feature_id: u16,
    answers: [Vec<u8>; 2],
    broadcast: [u8; 3],
    source: HidppEventSource,
) -> (Option<BatteryInfo>, Option<BatteryInfo>) {
    const PRODUCT_ID: u16 = 0xc08b;
    let fixture = battery_only_fixture(feature_id, PRODUCT_ID, "G502 HERO", &answers);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid battery replay"),
    );
    let (notifier, mut events, observed) = observed_event_channel();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_event_notifier(notifier);
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    let (online, before) = battery_node_reading(&mut enumerator, PRODUCT_ID).await;
    assert!(online, "{feature_id:#06x} node listed online");

    let mut report = vec![0x11, 0xff, 0x02, 0x00];
    report.extend_from_slice(&broadcast);
    report.extend_from_slice(&[0; 13]);
    assert_eq!(
        backend
            .emit_channel_report(HEADSET_CHANNEL, &report)
            .expect("known channel"),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), observed.wait_for(1))
        .await
        .expect("the broadcast must be recognised as a reconciliation request");
    assert_eq!(events.try_recv(), Ok(source));

    let (_, after) = battery_node_reading(&mut enumerator, PRODUCT_ID).await;
    backend
        .require_complete()
        .expect("every scripted battery exchange consumed");
    (before, after)
}

/// The `(percentage, status)` of a reading, for the assertions below. These
/// 0x1000/0x1001 readings always carry a percentage.
fn charge(battery: Option<BatteryInfo>) -> Option<(u8, BatteryStatus)> {
    battery.map(|battery| {
        let percentage = battery
            .percentage
            .expect("a 0x1000/0x1001 reading carries a percentage");
        (percentage, battery.status)
    })
}

#[tokio::test]
async fn legacy_battery_broadcast_requests_reconciliation_to_a_fresh_reading() {
    // 0x1000 `getBatteryLevelStatus`: level 50 %, next 20 %, discharging; the
    // broadcast reports the drop to 20 %, which the reconciliation re-reads.
    let (before, after) = battery_broadcast_refresh(
        0x1000,
        [
            short(0xff, 0x02, 0x00, [50, 20, 0x00]),
            short(0xff, 0x02, 0x00, [20, 5, 0x00]),
        ],
        [20, 5, 0x00],
        HidppEventSource::BatteryStatus,
    )
    .await;
    assert_eq!(
        (charge(before), charge(after)),
        (
            Some((50, BatteryStatus::Discharging)),
            Some((20, BatteryStatus::Discharging))
        )
    );
}

#[tokio::test]
async fn legacy_battery_broadcast_reports_a_charging_change() {
    // Plugged in: only the status byte moves to 1 (recharging), so the
    // reconciliation must surface "charging" with the same 50 %.
    let (before, after) = battery_broadcast_refresh(
        0x1000,
        [
            short(0xff, 0x02, 0x00, [50, 20, 0x00]),
            short(0xff, 0x02, 0x00, [50, 20, 0x01]),
        ],
        [50, 20, 0x01],
        HidppEventSource::BatteryStatus,
    )
    .await;
    assert_eq!(
        (charge(before), charge(after)),
        (
            Some((50, BatteryStatus::Discharging)),
            Some((50, BatteryStatus::Charging))
        )
    );
}

#[tokio::test]
async fn legacy_battery_broadcast_reports_charging_while_holding_the_percentage() {
    // Plugged in, and the firmware stops gauging: 0 % while recharging. The
    // pre-charge 50 % is held, but the status must still change.
    let (before, after) = battery_broadcast_refresh(
        0x1000,
        [
            short(0xff, 0x02, 0x00, [50, 20, 0x00]),
            short(0xff, 0x02, 0x00, [0, 0, 0x01]),
        ],
        [0, 0, 0x01],
        HidppEventSource::BatteryStatus,
    )
    .await;
    assert_eq!(
        (charge(before), charge(after)),
        (
            Some((50, BatteryStatus::Discharging)),
            Some((50, BatteryStatus::Charging))
        )
    );
}

#[tokio::test]
async fn voltage_battery_broadcast_requests_reconciliation_to_a_fresh_reading() {
    // 0x1001 `getBatteryInfo`: 3.70 V on battery, then 4.10 V after the
    // broadcast; the percentage is estimated from the voltage.
    let (before, after) = battery_broadcast_refresh(
        0x1001,
        [voltage_reading(3700, 0x00), voltage_reading(4100, 0x00)],
        [0x10, 0x04, 0x00],
        HidppEventSource::BatteryVoltage,
    )
    .await;
    assert!(
        matches!(
            (charge(before.clone()), charge(after.clone())),
            (
                Some((before, BatteryStatus::Discharging)),
                Some((after, BatteryStatus::Discharging))
            ) if after > before
        ),
        "{before:?} -> {after:?}"
    );
}

#[tokio::test]
async fn voltage_battery_broadcast_reports_charging_while_holding_the_percentage() {
    // Plugged in: flags bit 7 (external power), status 0 (charging). A
    // voltage under charge is no gauge, so the pre-charge estimate is held,
    // but the status must still change.
    let (before, after) = battery_broadcast_refresh(
        0x1001,
        [voltage_reading(3700, 0x00), voltage_reading(3900, 0x80)],
        [0x0f, 0x3c, 0x80],
        HidppEventSource::BatteryVoltage,
    )
    .await;
    let (percentage, status) = charge(before).expect("a reading before the broadcast");
    assert_eq!(status, BatteryStatus::Discharging);
    assert_eq!(charge(after), Some((percentage, BatteryStatus::Charging)));
}

/// Probe a [`setting_fixture`] node carrying `feature_id` (at index 3), emit
/// that feature's long `broadcast` (function, then payload) on its channel,
/// and return the node's settings revision before and after the
/// reconciliation it requests.
async fn setting_broadcast_revisions(
    feature_id: u16,
    broadcast: [u8; 4],
    source: HidppEventSource,
) -> (u64, u64) {
    const PRODUCT_ID: u16 = 0xb369;
    // One battery read rides the probe, one the reconciliation.
    let fixture = setting_fixture(feature_id, PRODUCT_ID, "Settings Test Device", 2);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid settings replay"),
    );
    let (notifier, mut events, observed) = observed_event_channel();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_event_notifier(notifier);
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    let revision = async |enumerator: &mut Enumerator| {
        let inventories = enumerator.enumerate().await.expect("node probe succeeds");
        let node = inventories
            .iter()
            .find(|inventory| inventory.receiver.product_id == PRODUCT_ID)
            .unwrap_or_else(|| panic!("node listed: {inventories:#?}"));
        node.paired[0].settings_revision
    };
    let before = revision(&mut enumerator).await;

    let [function, payload @ ..] = broadcast;
    let mut report = vec![0x11, 0xff, 0x03, function << 4];
    report.extend_from_slice(&payload);
    report.extend_from_slice(&[0; 13]);
    assert_eq!(
        backend
            .emit_channel_report(HEADSET_CHANNEL, &report)
            .expect("known channel"),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), observed.wait_for(1))
        .await
        .expect("the broadcast must be recognised as a reconciliation request");
    assert_eq!(events.try_recv(), Ok(source));

    let after = revision(&mut enumerator).await;
    backend
        .require_complete()
        .expect("the feature walk is the whole script");
    (before, after)
}

#[tokio::test]
async fn fn_lock_toggled_on_the_keyboard_moves_its_settings_revision() {
    // Fn+Esc on an MX Keys: 0x40a3 reports the new state unsolicited. The
    // payload is not decoded — the revision only says a re-read is due.
    assert_eq!(
        setting_broadcast_revisions(
            0x40a3,
            [0x00, 0x00, 0x01, 0x00],
            HidppEventSource::FnInversion
        )
        .await,
        (0, 1)
    );
}

#[tokio::test]
async fn a_dpi_button_moves_the_mouses_settings_revision() {
    // 0x2202 event 0: sensor 0 now at 1600 DPI.
    assert_eq!(
        setting_broadcast_revisions(
            0x2202,
            [0x00, 0x00, 0x06, 0x40],
            HidppEventSource::ExtendedDpi
        )
        .await,
        (0, 1)
    );
}

#[tokio::test]
async fn the_first_lock_key_broadcast_names_the_key_against_the_state_read_on_connection() {
    const PRODUCT_ID: u16 = 0xb36a;
    // Caps Lock is on when the keyboard connects. One battery read rides each
    // enumeration.
    let fixture = lock_key_fixture(PRODUCT_ID, "Lock Test Keyboard", 2, 0x02);
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![fixture.node],
                channels: vec![fixture.channel],
            },
            vec![fixture.cassette],
        )
        .expect("valid lock-key replay"),
    );
    let (notifier, mut events) = event_channel();
    let mut lock_keys = events.take_lock_keys().expect("lock keys not yet taken");
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_event_notifier(notifier);
    enumerator.timeouts.arrival_drain = Duration::ZERO;
    enumerator.enumerate().await.expect("node probe succeeds");
    // The second pass reuses the cached probe and reads the lock state no
    // more: the device is still connected.
    enumerator.enumerate().await.expect("cache hit succeeds");
    backend
        .require_complete()
        .expect("one lock-state read per connection");

    // Caps Lock off, broadcast on the lock-key index.
    let mut report = vec![0x11, 0xff, 0x03, 0x00];
    report.extend_from_slice(&[0; 16]);
    assert_eq!(
        backend
            .emit_channel_report(HEADSET_CHANNEL, &report)
            .expect("known channel"),
        1
    );
    let update = tokio::time::timeout(Duration::from_secs(2), lock_keys.recv())
        .await
        .expect("the broadcast must name the key")
        .expect("the notifier is alive");
    assert_eq!((update.key, update.engaged), (LockKey::CapsLock, false));
    // Lock keys are not inventory.
    assert_eq!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

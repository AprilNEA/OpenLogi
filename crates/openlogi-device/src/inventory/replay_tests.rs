use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::Enumerator;
use super::cache::{CACHE_MISS_GRACE, CacheKey};
use super::events::{HidppEventSource, observed_event_channel};
use super::replay_test_support::{
    BOLT_CHANNEL, BOLT_UID, BoltSlot, DIRECT_CHANNEL, bolt_fixture, connection_notification,
    direct_fixture, malformed_dpi_fixture, short,
};
use crate::fixture::{
    ChannelConnection, NodePresence, OpenOutcome, ReplayBackend, ReplayTopology, RequestMatch,
};
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
    let fixture = bolt_fixture(&slots, 1);
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
    enumerator.arrival_drain = Duration::ZERO;

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
    enumerator.arrival_drain = Duration::ZERO;
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
    enumerator.arrival_drain = Duration::ZERO;
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

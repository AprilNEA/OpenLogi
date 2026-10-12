//! A real capture teardown on an injected transport, followed by route migration.

use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use openlogi_hid::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};
use openlogi_hid::{ChannelRegistry, Enumerator, NodeId, NodeInfo};

use super::*;
use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};

const CHANNEL: &str = "migrating-mouse";
const PRODUCT_ID: u16 = 0xb35b;
const HAPTIC_CID: u16 = 0x01a0;

fn return_node() -> NodeId {
    NodeId::from("returned-mouse-node".to_owned())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One replay follows capture, migration, retained cleanup, return and shutdown in order"
)]
async fn route_migration_preserves_restore_without_blocking_new_transport() {
    let direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: PRODUCT_ID,
    };
    let bolt = DeviceRoute::Bolt {
        receiver_uid: "fixture-receiver".to_owned(),
        slot: 2,
    };
    let node = NodeId::from("migrating-node".to_owned());
    let backend = Arc::new(ReplayBackend::new(topology(node.clone()), vec![cassette()]).unwrap());
    let registry = ChannelRegistry::default();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_registry(registry.clone());
    enumerator.enumerate().await.unwrap();
    let armed = backend
        .hold_next_response(
            CHANNEL,
            RequestMatch::Hidpp20,
            &[0x10, 0xff, 0, 0, 0x1d, 0x4b, 0],
        )
        .unwrap();
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let access = ReceiverAccess::default();
    let device_access = DeviceAccess {
        channel: CaptureChannelSlot::default(),
        registry,
        receiver_access: access.clone(),
        device_io,
    };
    let (ring, _ring_rx) = mpsc::unbounded_channel();
    let mut actions =
        crate::runtime::ActionRuntime::new(Arc::default(), device_access.clone(), ring).unwrap();
    let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::default(),
    )))
    .unwrap();
    let mut state = GestureManagerState::new(GestureOutputs::new(
        actions.dispatcher(),
        scroll.input(),
        Arc::default(),
    ));
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let channels = SessionChannels {
        events,
        access: device_access,
    };
    let first = Arc::new(vec![capture_plan(direct.clone())]);
    assert!(
        first[0]
            .target
            .spec
            .divert_buttons
            .contains(&(HAPTIC_CID, ButtonId::HapticPanel))
    );
    let (plans_tx, plans) = watch::channel(first.clone());
    let requests = access.subscribe_requests();
    state
        .reconcile(*requests.borrow(), true, &first, &channels)
        .await;
    armed.request_written().await;

    // Drive the production publisher and session teardown. The vanished route
    // cannot restore its diverted panel, so ordered Done carries a real token.
    backend
        .set_node_presence(&node, NodePresence::Absent)
        .unwrap();
    backend
        .set_channel_connection(CHANNEL, ChannelConnection::Disconnected)
        .unwrap();
    enumerator.enumerate().await.unwrap();
    let second = Arc::new(vec![capture_plan(bolt.clone())]);
    plans_tx.send(second.clone()).unwrap();
    state
        .reconcile(*requests.borrow(), true, &second, &channels)
        .await;
    assert_eq!(
        state
            .slots
            .values()
            .filter_map(GestureSlot::session)
            .count(),
        1,
        "the old epoch must finish before the new transport starts capture"
    );
    assert_eq!(
        state
            .slots
            .values()
            .find_map(GestureSlot::session)
            .unwrap()
            .target()
            .route,
        direct
    );
    armed.release();
    let done = event_rx
        .recv()
        .await
        .expect("retired capture must report ordered Done");
    assert!(matches!(&done, SessionEvent::Done(done) if done.pending_restore.is_some()));
    state.handle_session_event(done, true, &requests, &plans);
    state
        .reconcile(*requests.borrow(), true, &second, &channels)
        .await;

    assert!(
        state.has_pending_restores(),
        "the old route's firmware debt must survive migration"
    );
    let successor = state
        .slots
        .values()
        .find_map(GestureSlot::session)
        .expect("an absent Bluetooth route must not block capture on Bolt");
    assert_eq!(successor.target().route, bolt);
    assert!(
        state.deadline(*requests.borrow(), true, &second).is_none(),
        "an inactive route must not schedule cleanup retries while this mouse uses another transport"
    );
    assert!(
        state.deadline(*requests.borrow(), true, &first).is_some(),
        "returning publication must make cleanup eligible before another reconcile"
    );

    // The Bolt attempt has no published channel in this fixture. Its normal
    // failure provides ordered completion without manufacturing private state.
    let done = event_rx.recv().await.unwrap();
    state.handle_session_event(done, true, &requests, &plans);
    let returned = Arc::new(vec![capture_plan(direct.clone())]);
    plans_tx.send(returned.clone()).unwrap();
    state
        .reconcile(*requests.borrow(), true, &returned, &channels)
        .await;
    assert!(state.has_pending_restores());
    assert!(
        state.slots.values().all(|slot| slot.session().is_none()),
        "returning to Bluetooth must wait for that route's outstanding restore"
    );
    let completion = backend.channel_completion(CHANNEL).unwrap();
    assert!(
        completion.unmatched_requests.is_empty(),
        "{:?}",
        completion.unmatched_requests
    );
    assert_eq!(
        completion
            .written_reports
            .iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .count(),
        1,
        "only arming was written; cleanup must not use a vanished transport"
    );

    // Return as a fresh OS node. Registry publication accelerates cleanup on
    // the original route; it must finish before that route is armed again.
    backend
        .set_node_presence(&return_node(), NodePresence::Present)
        .unwrap();
    backend
        .set_channel_connection(CHANNEL, ChannelConnection::Connected)
        .unwrap();
    enumerator.enumerate().await.unwrap();
    plans_tx.send(second.clone()).unwrap();
    state.expedite_pending_restores();
    let lease = access.try_acquire_for_session().unwrap();
    retry_pending_restores(
        &mut state.slots,
        &channels.access.registry,
        Instant::now(),
        &second,
    )
    .await;
    drop(lease);
    assert!(state.deadline(*requests.borrow(), true, &second).is_none());
    assert_eq!(
        backend
            .channel_completion(CHANNEL)
            .unwrap()
            .written_reports
            .iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .count(),
        1,
        "even an available old channel must stay dormant while another route is wanted"
    );
    plans_tx.send(returned.clone()).unwrap();
    let rearmed = backend
        .hold_next_response(
            CHANNEL,
            RequestMatch::Hidpp20,
            &[0x10, 0xff, 0, 0, 0x1d, 0x4b, 0],
        )
        .unwrap();
    state.expedite_pending_restores();
    state
        .reconcile(*requests.borrow(), true, &returned, &channels)
        .await;
    rearmed.request_written().await;
    assert!(!state.has_pending_restores());
    assert_eq!(
        state
            .slots
            .values()
            .find_map(GestureSlot::session)
            .unwrap()
            .target()
            .route,
        direct
    );

    let empty = Arc::new(Vec::new());
    plans_tx.send(empty.clone()).unwrap();
    state
        .reconcile(*requests.borrow(), true, &empty, &channels)
        .await;
    rearmed.release();
    let done = event_rx.recv().await.unwrap();
    assert!(matches!(&done, SessionEvent::Done(done) if done.pending_restore.is_none()));
    state.handle_session_event(done, true, &requests, &plans);
    assert!(state.slots.is_empty());
    let writes = backend.channel_completion(CHANNEL).unwrap().written_reports;
    assert_eq!(
        writes
            .iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .map(|report| report[6])
            .collect::<Vec<_>>(),
        [0x23, 0x22, 0x23, 0x22],
        "return cleanup precedes rearm; normal shutdown releases the new capture"
    );
    backend.require_complete().unwrap();
    scroll.shutdown();
    actions.shutdown();
}

fn capture_plan(route: DeviceRoute) -> DeviceCapturePlan {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::HapticPanel,
        Binding::Single(Action::MissionControl),
    );
    crate::capture_plan::plan_for_device(&config, physical_key(), "mouse-a", route, None, 0, true)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Follow a real armed capture through migration while its old publication stays present"
)]
async fn route_migration_defers_teardown_on_a_still_published_channel() {
    let node = NodeId::from("migrating-node".to_owned());
    let mut fixture = cassette();
    let last_setup = fixture
        .exchanges
        .iter()
        .position(|exchange| exchange.request == [0x10, 0xff, 0, 0, 0x1d, 0x4b, 0])
        .unwrap();
    let mut restore = fixture
        .exchanges
        .iter()
        .find(|exchange| {
            exchange.request.len() == 20 && exchange.request[2] == 2 && exchange.request[6] == 0x22
        })
        .unwrap()
        .clone();
    restore.required = false;
    fixture.exchanges.truncate(last_setup + 1);
    fixture.exchanges.push(restore);
    let backend = Arc::new(ReplayBackend::new(topology(node), vec![fixture]).unwrap());
    let registry = ChannelRegistry::default();
    let mut enumerator = Enumerator::with_backend(backend.clone()).with_registry(registry.clone());
    enumerator.enumerate().await.unwrap();
    let armed = backend
        .hold_next_response(
            CHANNEL,
            RequestMatch::Hidpp20,
            &[0x10, 0xff, 0, 0, 0x1d, 0x4b, 0],
        )
        .unwrap();
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let access = ReceiverAccess::default();
    let device_access = DeviceAccess {
        channel: CaptureChannelSlot::default(),
        registry,
        receiver_access: access.clone(),
        device_io,
    };
    let (ring, _ring_rx) = mpsc::unbounded_channel();
    let mut actions =
        crate::runtime::ActionRuntime::new(Arc::default(), device_access.clone(), ring).unwrap();
    let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::default(),
    )))
    .unwrap();
    let mut state = GestureManagerState::new(GestureOutputs::new(
        actions.dispatcher(),
        scroll.input(),
        Arc::default(),
    ));
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let channels = SessionChannels {
        events,
        access: device_access,
    };
    let first = Arc::new(vec![capture_plan(DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: PRODUCT_ID,
    })]);
    let (plans_tx, plans) = watch::channel(first.clone());
    let requests = access.subscribe_requests();
    state
        .reconcile(*requests.borrow(), true, &first, &channels)
        .await;
    armed.request_written().await;
    let second = Arc::new(vec![capture_plan(DeviceRoute::Bolt {
        receiver_uid: "fixture-receiver".to_owned(),
        slot: 2,
    })]);
    plans_tx.send(second.clone()).unwrap();
    state
        .reconcile(*requests.borrow(), true, &second, &channels)
        .await;
    armed.release();
    let done = event_rx.recv().await.unwrap();
    assert!(
        matches!(&done, SessionEvent::Done(done) if done.pending_restore.is_some()),
        "migration must hand back deferred cleanup, even when the old channel remains published"
    );
    state.handle_session_event(done, true, &requests, &plans);
    state
        .reconcile(*requests.borrow(), true, &second, &channels)
        .await;
    assert_eq!(
        backend
            .channel_completion(CHANNEL)
            .unwrap()
            .written_reports
            .iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .count(),
        1,
        "migration must not wait for writes on a transport the mouse has left"
    );
    state.handle_session_event(event_rx.recv().await.unwrap(), true, &requests, &plans);

    // Returning to the same channel (the receiver never unplugged) must still
    // permit its original debt to be restored before any successor uses it.
    let lease = access.try_acquire_for_session().unwrap();
    state.expedite_pending_restores();
    retry_pending_restores(
        &mut state.slots,
        &channels.access.registry,
        Instant::now(),
        &first,
    )
    .await;
    drop(lease);
    assert!(!state.has_pending_restores());
    let completion = backend.channel_completion(CHANNEL).unwrap();
    assert_eq!(
        completion
            .written_reports
            .iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .map(|report| report[6])
            .collect::<Vec<_>>(),
        [0x23, 0x22]
    );
    backend.require_complete().unwrap();
    scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Both switch directions exercise ordered retirement, a bounce and stale completion on the real manager"
)]
async fn rapid_route_switch_waits_for_done_and_ignores_stale_completion() {
    let direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: PRODUCT_ID,
    };
    let bolt = DeviceRoute::Bolt {
        receiver_uid: "fixture-receiver".to_owned(),
        slot: 2,
    };
    for (before, after) in [(direct.clone(), bolt.clone()), (bolt, direct)] {
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let access = ReceiverAccess::default();
        let device_access = DeviceAccess {
            channel: CaptureChannelSlot::default(),
            registry: ChannelRegistry::default(),
            receiver_access: access.clone(),
            device_io,
        };
        let (ring, _ring_rx) = mpsc::unbounded_channel();
        let mut actions =
            crate::runtime::ActionRuntime::new(Arc::default(), device_access.clone(), ring)
                .unwrap();
        let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
            false,
            VerticalScrollSensitivity::default(),
        )))
        .unwrap();
        let mut state = GestureManagerState::new(GestureOutputs::new(
            actions.dispatcher(),
            scroll.input(),
            Arc::default(),
        ));
        let (events, mut event_rx) = mpsc::unbounded_channel();
        let channels = SessionChannels {
            events,
            access: device_access,
        };
        let old_plan = capture_plan(before);
        let key = CaptureKey::for_target(&old_plan.target);
        let id = session_id(7);
        let (stop, mut stopped) = oneshot::channel();
        state.slots.insert(
            key.clone(),
            GestureSlot::running(CaptureSession::active(
                id.clone(),
                old_plan.target.clone(),
                old_plan.dispatch.clone(),
                stop,
            )),
        );
        let new = Arc::new(vec![capture_plan(after.clone())]);
        let (plans_tx, plans) = watch::channel(new.clone());
        let requests = access.subscribe_requests();
        state
            .reconcile(*requests.borrow(), true, &new, &channels)
            .await;
        assert_eq!(
            stopped
                .try_recv()
                .expect("migration must retire the original epoch"),
            openlogi_hid::CaptureShutdown::Moved
        );

        let bounced = Arc::new(vec![old_plan]);
        plans_tx.send(bounced.clone()).unwrap();
        state
            .reconcile(*requests.borrow(), true, &bounced, &channels)
            .await;
        plans_tx.send(new.clone()).unwrap();
        state
            .reconcile(*requests.borrow(), true, &new, &channels)
            .await;
        assert_eq!(
            state.slots.len(),
            1,
            "rapid switching must not overlap capture epochs"
        );
        assert!(!state.slots[&key].session().unwrap().is_active());
        state.handle_session_event(
            SessionEvent::Done(SessionDone {
                key: key.clone(),
                session: id.clone(),
                pending_restore: None,
            }),
            true,
            &requests,
            &plans,
        );
        state
            .reconcile(*requests.borrow(), true, &new, &channels)
            .await;
        let successor = state
            .slots
            .values()
            .find_map(GestureSlot::session)
            .unwrap()
            .id()
            .clone();
        assert_eq!(
            state
                .slots
                .values()
                .find_map(GestureSlot::session)
                .unwrap()
                .target()
                .route,
            after
        );
        assert!(
            !state.handle_session_event(
                SessionEvent::Done(SessionDone {
                    key,
                    session: id,
                    pending_restore: None,
                }),
                true,
                &requests,
                &plans
            ),
            "stale Done must not change a different route's successor"
        );
        assert!(
            state
                .slots
                .values()
                .find_map(GestureSlot::session)
                .unwrap()
                .owns(&successor)
        );
        state.handle_session_event(event_rx.recv().await.unwrap(), true, &requests, &plans);
        scroll.shutdown();
        actions.shutdown();
    }
}

fn topology(node: NodeId) -> ReplayTopology {
    let first = ReplayNode {
        info: NodeInfo {
            id: node,
            vendor_id: 0x046d,
            product_id: PRODUCT_ID,
            usage_page: 0xff00,
            usage_id: 2,
            name: "Replay Mouse".to_owned(),
            manufacturer: None,
            serial_number: None,
        },
        presence: NodePresence::Present,
        open_outcome: OpenOutcome::Hidpp,
        channel: Some(CHANNEL.to_owned()),
        raw_writer: RawWriterAvailability::Unavailable,
        receiver_slots: Vec::new(),
    };
    let mut returned = first.clone();
    returned.info.id = return_node();
    returned.presence = NodePresence::Absent;
    ReplayTopology {
        nodes: vec![first, returned],
        channels: vec![ReplayChannel {
            id: CHANNEL.to_owned(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

fn cassette() -> HidCassette {
    let short = |feature: u8, function: u8, payload: [u8; 3]| {
        let mut report = vec![0x10, 0xff, feature, function << 4];
        report.extend(payload);
        report
    };
    let long = |function: u8, payload: [u8; 16]| {
        let mut report = vec![0x11, 0xff, 2, function << 4];
        report.extend(payload);
        report
    };
    let mut info = [0; 16];
    info[..2].copy_from_slice(&HAPTIC_CID.to_be_bytes());
    info[4] = 0x20; // divertable; plain button capture does not need raw XY
    let mut reporting = [0; 16];
    reporting[..2].copy_from_slice(&HAPTIC_CID.to_be_bytes());
    let mut diverted = reporting;
    diverted[2] = 0x23; // diversion valid/on, raw XY valid/off
    let exchange = |request, response| CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    };
    let probe = vec![
        exchange(short(0, 1, [0, 0, 0]), short(0, 1, [4, 0, 0])),
        exchange(short(0, 0, [0, 1, 0]), short(0, 0, [1, 0, 0])),
        exchange(short(1, 0, [0, 0, 0]), short(1, 0, [2, 0, 0])),
        exchange(short(1, 1, [1, 0, 0]), short(1, 1, [0, 1, 0])),
        exchange(short(1, 1, [2, 0, 0]), short(1, 1, [0x1b, 4, 0])),
        exchange(short(2, 0, [0, 0, 0]), short(2, 0, [1, 0, 0])),
        exchange(long(1, [0; 16]), long(1, info)),
    ];
    let capture = vec![
        exchange(short(0, 1, [0, 0, 0]), short(0, 1, [4, 0, 0])),
        exchange(short(0, 0, [0x1b, 4, 0]), short(0, 0, [2, 0, 4])),
        exchange(short(2, 0, [0, 0, 0]), short(2, 0, [1, 0, 0])),
        exchange(long(1, [0; 16]), long(1, info)),
        exchange(short(2, 2, [1, 0xa0, 0]), long(2, reporting)),
        exchange(long(3, diverted), long(3, diverted)),
        exchange(short(0, 0, [0x1d, 0x4b, 0]), short(0, 0, [0, 0, 0])),
    ];
    let mut restored = reporting;
    restored[2] = 0x22;
    let restore = exchange(long(3, restored), long(3, restored));
    let mut exchanges = probe.clone();
    exchanges.extend(capture.clone());
    exchanges.extend(probe);
    exchanges.push(restore.clone());
    exchanges.extend(capture);
    exchanges.push(restore);
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "capture route migration".to_owned(),
        channel: CHANNEL.to_owned(),
        report_support: ReportSupport::ShortAndLong,
        exchanges,
    }
}

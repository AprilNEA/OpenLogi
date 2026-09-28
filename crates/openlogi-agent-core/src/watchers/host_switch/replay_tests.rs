//! Multi-keyboard recovery uses a real pending token from a replay session.

use super::*;
use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use openlogi_hid::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};
use openlogi_hid::{KeyboardHostTransition, NodeId, NodeInfo, ReportedHostSlot};
use std::sync::Arc;

#[tokio::test]
async fn absent_keyboard_cleanup_does_not_block_another_keyboards_departure() {
    let first = HostSwitchLink {
        keyboard: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        },
        ..super::tests::link(2)
    };
    let second = HostSwitchLink {
        keyboard_key: "second-keyboard".into(),
        keyboard: super::tests::route(3),
        targets: vec![super::tests::route(4)],
    };
    let token = pending_restore_for(&first).await;
    let mut state = HostSwitchManagerState::new();
    state.slots.push(HostSwitchSlot::Recovering(Recovery {
        link: first.clone(),
        epoch: SessionEpoch(1),
        requested_host: None,
        restore: RestorePhase::Ready {
            token,
            retry_at: Instant::now() + RETRY_DELAY,
        },
    }));
    let (stop, _stopped) = oneshot::channel();
    state.slots.push(HostSwitchSlot::Running(RunningSession {
        link: second.clone(),
        epoch: SessionEpoch(2),
        phase: SessionPhase::Active(stop),
    }));
    let request = HostSwitchRequest {
        host: 1,
        keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        },
    };
    state.handle_session_completion(
        SessionCompletion {
            epoch: SessionEpoch(2),
            result: Ok(SessionResult {
                requested_host: Some(request),
                pending_restore: None,
                failed: false,
            }),
        },
        &[first.clone(), second.clone()],
        false,
    );

    let intent = state
        .begin_transition(false)
        .expect("unrelated cleanup must not block forwarding");
    assert_eq!(intent.link, second);
    assert_eq!(intent.request, request);
    assert!(
        state.owns_keyboard(&first.keyboard),
        "pending cleanup still prevents rearming the first keyboard"
    );
    assert!(
        state.has_pending_restores(),
        "forwarding cannot discard firmware ownership"
    );
    handle_manager_event(&mut state, ManagerEvent::Transition(Ok(())), &[], false);
    assert!(state.has_pending_restores());
    assert!(state.terminal_completion(true).is_none());
}

async fn pending_restore_for(link: &HostSwitchLink) -> PendingHostSwitchRestore {
    let node = NodeInfo {
        id: NodeId::from("host-switch-recovery".to_owned()),
        vendor_id: 0x046d,
        product_id: 0xb35b,
        usage_page: 0xff00,
        usage_id: 2,
        name: "Synthetic host-switch keyboard".into(),
        manufacturer: None,
        serial_number: None,
    };
    let backend = Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![ReplayNode {
                    info: node.clone(),
                    presence: NodePresence::Present,
                    open_outcome: OpenOutcome::Hidpp,
                    channel: Some("keyboard".into()),
                    raw_writer: RawWriterAvailability::Unavailable,
                    receiver_slots: Vec::new(),
                }],
                channels: vec![ReplayChannel {
                    id: "keyboard".into(),
                    connection: ChannelConnection::Connected,
                    report_support: ReportSupport::ShortAndLong,
                }],
            },
            vec![restore_failure_cassette()],
        )
        .unwrap(),
    );
    let registry = ChannelRegistry::default();
    let mut enumerator =
        openlogi_hid::Enumerator::with_backend(backend.clone()).with_registry(registry.clone());
    assert_eq!(enumerator.enumerate().await.unwrap().len(), 1);
    let (stop, stopped) = oneshot::channel();
    stop.send(HostSwitchStopReason::Graceful).unwrap();
    let (_signal, gate) = openlogi_hid::device_io_channel();
    let result = run_host_switch_session(
        link.keyboard.clone(),
        stopped,
        &registry,
        HostSwitchCaptureMode::Full,
        gate,
    )
    .await
    .unwrap();
    let (request, pending) = result.into_parts();
    assert!(request.is_none());
    backend
        .set_node_presence(&node.id, NodePresence::Absent)
        .unwrap();
    backend
        .set_channel_connection("keyboard", ChannelConnection::Disconnected)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.lookup(&link.keyboard).is_some() {
            enumerator.enumerate().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("absent keyboard must retire from the registry");
    backend.require_complete().unwrap();
    pending.expect("failed teardown must retain a real restoration token")
}

fn restore_failure_cassette() -> HidCassette {
    let short = |feature: u8, function: u8, payload: &[u8]| {
        let mut report = vec![0x10, 0xff, feature, function << 4];
        report.extend_from_slice(payload);
        report.resize(7, 0);
        report
    };
    let long = |function: u8, payload: &[u8]| {
        let mut report = vec![0x11, 0xff, 2, function << 4];
        report.extend_from_slice(payload);
        report.resize(20, 0);
        report
    };
    let exchange = |request, response| CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    };
    let failure = vec![0x10, 0xff, 0xff, 2, 0x30, 0x08, 0];
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "host-switch restoration failure".into(),
        channel: "keyboard".into(),
        report_support: ReportSupport::ShortAndLong,
        exchanges: vec![
            exchange(short(0, 1, &[0, 0, 0]), short(0, 1, &[4, 0, 0])),
            exchange(short(0, 0, &[0, 1, 0]), short(0, 0, &[1, 0, 0])),
            exchange(short(1, 0, &[]), short(1, 0, &[2, 0, 0])),
            exchange(short(1, 1, &[1, 0, 0]), short(1, 1, &[0, 1, 0])),
            exchange(short(1, 1, &[2, 0, 0]), short(1, 1, &[0x1b, 4, 0])),
            exchange(short(2, 0, &[]), short(2, 0, &[1, 0, 0])),
            exchange(long(1, &[]), long(1, &[0, 0xd1, 0, 0, 0x20])),
            exchange(short(0, 1, &[0, 0, 0]), short(0, 1, &[4, 0, 0])),
            exchange(short(0, 0, &[0x18, 0x14, 0]), short(0, 0, &[0, 0, 0])),
            exchange(short(0, 0, &[0x1b, 4, 0]), short(0, 0, &[2, 0, 4])),
            exchange(short(2, 0, &[0, 0, 0]), short(2, 0, &[1, 0, 0])),
            exchange(long(1, &[]), long(1, &[0, 0xd1, 0, 0, 0x20])),
            exchange(short(2, 2, &[0, 0xd1, 0]), long(2, &[0, 0xd1, 0x54])),
            exchange(long(3, &[0, 0xd1, 0x23]), long(3, &[0, 0xd1, 0x23])),
            exchange(long(3, &[0, 0xd1, 0x32]), failure.clone()),
            exchange(long(3, &[0, 0xd1, 0x32]), failure),
        ],
    }
}

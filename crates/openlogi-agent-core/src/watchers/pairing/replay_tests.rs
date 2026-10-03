//! Replay-backed proof that the watcher uses its injected hardware context.

use std::sync::Arc;
use std::time::Duration;

use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use openlogi_hid::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};
use openlogi_hid::{
    NodeId, NodeInfo, PairingError, PairingEvent, ReceiverSelector, device_io_channel,
};

use super::{PairingControl, PairingSessionId, spawn_with_hardware};
use crate::hardware::HardwareContext;

const CHANNEL: &str = "agent-bolt-pairing";
const BOLT_PRODUCT_ID: u16 = 0xc548;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one replay follows admission, cancellation, firmware restoration, and channel release"
)]
async fn driver_handoff_skips_a_reserved_receiver_and_waits_for_pairing_restoration() {
    use crate::peripherals::ownership::Owner;
    use openlogi_core::peripheral::{EndpointId, SessionId};
    use std::collections::HashSet;
    let node = NodeId::from("pairing-owner".to_owned());
    let blocked = NodeId::from("plugin-owner".to_owned());
    let mut topology = bolt_topology(node.clone());
    let mut reserved = topology.nodes[0].clone();
    reserved.info.id = blocked.clone();
    topology.nodes.insert(0, reserved);
    let backend =
        Arc::new(ReplayBackend::new(topology, vec![bolt_pairing_cancel_cassette()]).unwrap());
    let cleanup = backend
        .hold_next_response(
            CHANNEL,
            RequestMatch::Exact,
            &receiver_notification_flags_write([0, 0, 0]),
        )
        .unwrap();
    let (_signal, gate) = device_io_channel();
    let hardware = HardwareContext::injected(backend.clone(), gate);
    let ownership = hardware.ownership();
    let blocked_session = SessionId {
        endpoint: EndpointId("blocked".into()),
        generation: 1,
    };
    ownership
        .reserve(
            &blocked_session,
            HashSet::from([blocked.clone()]),
            Vec::new(),
        )
        .unwrap();
    let reporters: Vec<_> = [
        Owner::Gesture,
        Owner::Keyboard,
        Owner::HostSwitch,
        Owner::Runtime,
        Owner::Inventory,
    ]
    .into_iter()
    .map(|owner| ownership.owner(owner))
    .collect();
    let (control, mut events) = spawn_with_hardware(hardware);
    let pairing = PairingSessionId::new(11);
    control
        .send(PairingControl::Start {
            session: pairing,
            selector: ReceiverSelector::First,
        })
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(started.event, PairingEvent::Searching));
    assert_eq!(
        backend.open_count(&blocked).unwrap(),
        0,
        "pairing must not probe the plugin-owned receiver"
    );
    let replacement = SessionId {
        endpoint: EndpointId("replacement".into()),
        generation: 2,
    };
    ownership
        .reserve(&replacement, HashSet::from([node]), Vec::new())
        .unwrap();
    for reporter in &reporters {
        reporter.observe(&ownership.requests(), Vec::new(), HashSet::new());
    }
    assert!(
        !ownership.ready(&replacement),
        "an admitted pairing session still owns its receiver"
    );
    control
        .send(PairingControl::Cancel { session: pairing })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), cleanup.request_written())
        .await
        .unwrap();
    assert!(
        !ownership.ready(&replacement),
        "the final firmware restore is still in flight"
    );
    assert!(
        ownership.requests().excluded().is_empty(),
        "restore must finish before channel retirement"
    );
    cleanup.release();
    let completed = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        completed.event,
        PairingEvent::Failed(PairingError::Cancelled)
    ));
    assert!(
        !ownership.ready(&replacement),
        "inventory must acknowledge retirement separately"
    );
    for reporter in &reporters {
        reporter.observe(&ownership.requests(), Vec::new(), HashSet::new());
    }
    assert!(ownership.ready(&replacement));
    assert_eq!(backend.channel_lifetime_count(CHANNEL).unwrap(), 0);
    backend.require_complete().unwrap();
}

#[tokio::test]
async fn injected_pairing_waits_for_receiver_cleanup_before_terminal_failure() {
    let node_id = NodeId::from("agent-bolt-pairing-node".to_string());
    let backend = Arc::new(
        ReplayBackend::new(
            bolt_topology(node_id.clone()),
            vec![bolt_pairing_cancel_cassette()],
        )
        .expect("valid agent pairing replay fixture"),
    );
    let cleanup = backend
        .hold_next_response(
            CHANNEL,
            RequestMatch::Exact,
            &receiver_notification_flags_write([0, 0, 0]),
        )
        .expect("notification cleanup response can be held");
    let (_device_io_signal, device_io) = device_io_channel();
    let hardware = HardwareContext::injected(backend.clone(), device_io);
    let (control, mut events) = spawn_with_hardware(hardware);
    let session = PairingSessionId::new(7);

    control
        .send(PairingControl::Start {
            session,
            selector: ReceiverSelector::First,
        })
        .expect("pairing watcher accepts a start");
    let searching = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("replay pairing reaches searching promptly")
        .expect("pairing watcher remains available");
    assert_eq!(searching.session, session);
    assert!(matches!(searching.event, PairingEvent::Searching));

    control
        .send(PairingControl::Cancel { session })
        .expect("searching pairing session accepts cancellation");
    tokio::time::timeout(Duration::from_secs(2), cleanup.request_written())
        .await
        .expect("pairing reaches notification cleanup promptly");
    assert!(
        events.try_recv().is_err(),
        "terminal failure must remain private until receiver cleanup completes"
    );

    cleanup.release();
    let terminal = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("terminal failure follows released cleanup promptly")
        .expect("pairing watcher remains available");
    assert_eq!(terminal.session, session);
    assert!(matches!(
        terminal.event,
        PairingEvent::Failed(PairingError::Cancelled)
    ));
    assert!(
        events.try_recv().is_err(),
        "searching and one typed failure are the complete event sequence"
    );

    assert_eq!(backend.open_count(&node_id).expect("known replay node"), 1);
    let completion = backend
        .channel_completion(CHANNEL)
        .expect("known replay channel");
    assert_eq!(
        completion.written_reports,
        vec![
            receiver_notification_flags_write([0, 0x09, 0]),
            bolt_discovery_write([30, 0x01, 0]),
            bolt_discovery_write([30, 0x02, 0]),
            receiver_notification_flags_write([0, 0, 0]),
        ]
    );
    assert_eq!(completion.channel_open_count, 1);
    assert_eq!(
        backend
            .channel_lifetime_count(CHANNEL)
            .expect("known replay channel"),
        0,
        "the terminal event is published only after the receiver channel closes"
    );
    backend
        .require_complete()
        .expect("pairing cassette is strictly consumed");
}

fn bolt_topology(node_id: NodeId) -> ReplayTopology {
    ReplayTopology {
        nodes: vec![ReplayNode {
            info: NodeInfo {
                id: node_id,
                vendor_id: 0x046d,
                product_id: BOLT_PRODUCT_ID,
                usage_page: 0xff00,
                usage_id: 0x0002,
                name: "Agent Replay Bolt Receiver".to_string(),
                manufacturer: Some("Logitech".to_string()),
                serial_number: None,
            },
            presence: NodePresence::Present,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(CHANNEL.to_string()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: Vec::new(),
        }],
        channels: vec![ReplayChannel {
            id: CHANNEL.to_string(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

fn bolt_pairing_cancel_cassette() -> HidCassette {
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "agent Bolt pairing cancellation".to_string(),
        channel: CHANNEL.to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges: [
            receiver_notification_flags_write([0, 0x09, 0]),
            bolt_discovery_write([30, 0x01, 0]),
            bolt_discovery_write([30, 0x02, 0]),
            receiver_notification_flags_write([0, 0, 0]),
        ]
        .into_iter()
        .map(exact_echo)
        .collect(),
    }
}

fn receiver_notification_flags_write(flags: [u8; 3]) -> Vec<u8> {
    register_write(0x00, flags)
}

fn bolt_discovery_write(payload: [u8; 3]) -> Vec<u8> {
    register_write(0xc0, payload)
}

fn register_write(address: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, 0xff, 0x80, address, payload[0], payload[1], payload[2],
    ]
}

fn exact_echo(report: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Exact,
        request: report.clone(),
        response: Some(report),
        required: true,
    }
}

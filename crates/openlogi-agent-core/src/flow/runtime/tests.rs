//! A real peer endpoint drives a generation over loopback QUIC.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::FlowPublicKey;
use openlogi_core::device::DeviceKind;
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::transport::{
    FlowConnection, FlowEndpoint, MachineIdentity, NotificationEvent, PeerTrust, message_envelope,
};
use openlogi_ipc::FlowLinkState;
use tokio::sync::watch;

use super::{Binding, FlowRuntime, FlowStart, hello};
use crate::flow::devices::InventoryDevice;
use crate::flow::test_support::{
    KEYBOARD, config_with_key, configured, no_hardware, record, unit_identity,
};
use crate::observable::ObservableState;

const WAIT: Duration = Duration::from_secs(5);

fn loopback() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// The keyboard as this host sees it.
fn inventory(online: bool) -> Arc<[InventoryDevice]> {
    Arc::from([record(KEYBOARD, DeviceKind::Keyboard, 0x0b, online)])
}

fn request(transfer_id: u64) -> Envelope {
    let mut request = proto::HandoffRequest {
        transfer_id,
        devices: vec![unit_identity(0x0b)],
        ..Default::default()
    };
    let entry = request.entry.get_or_insert_default();
    entry.side = proto::Side::Left.into();
    entry.t = 0.5;
    message_envelope(FrameKind::HandoffRequest, &request).expect("encode the request")
}

async fn call(connection: &FlowConnection, transfer_id: u64) -> proto::HandoffAccept {
    let response = tokio::time::timeout(WAIT, connection.call(request(transfer_id)))
        .await
        .expect("the response arrives in time")
        .expect("the call succeeds");
    assert_eq!(response.kind, FrameKind::HandoffAccept);
    response
        .decode(InboundRole::Notification)
        .expect("decode the accept")
}

/// Skip the whole-state notifications until a handoff result arrives.
async fn next_result(connection: &FlowConnection) -> proto::HandoffResult {
    loop {
        let event = tokio::time::timeout(WAIT, connection.accept_notification())
            .await
            .expect("a notification arrives in time")
            .expect("the connection stays open");
        if let NotificationEvent::Notification(envelope) = event
            && envelope.kind == FrameKind::HandoffResult
        {
            return envelope
                .decode(InboundRole::Notification)
                .expect("decode the result");
        }
    }
}

async fn wait_for_link(observable: &ObservableState, state: FlowLinkState) {
    let mut observations = observable.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let reached = observable
                .snapshot()
                .flow
                .peers
                .first()
                .is_some_and(|peer| peer.state == state);
            if reached {
                return;
            }
            observations.changed().await.expect("the observable lives");
        }
    })
    .await
    .expect("the link state is published in time");
}

#[tokio::test]
async fn a_peer_hands_a_keyboard_to_this_host() {
    let local = MachineIdentity::generate().expect("local identity");
    let local_key = local.public_key();
    let remote = MachineIdentity::generate().expect("peer identity");
    let remote_key = remote.public_key();
    let config = config_with_key(
        FlowPublicKey::from_bytes(*remote_key.as_bytes()),
        vec![configured(KEYBOARD, Some(1))],
    );
    let (inventory_tx, inventory_rx) = watch::channel(inventory(false));
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    let (_device_io, hardware) = no_hardware();
    let runtime = FlowRuntime::start_on(
        FlowStart {
            config: Arc::new(config),
            inventory: inventory_rx,
            observable: Arc::clone(&observable),
            hardware,
        },
        Binding {
            identity: local,
            address: loopback(),
            mdns: false,
            // No pointer, as on Wayland or a headless host: receiving must
            // not depend on it.
            probe: false,
        },
    )
    .expect("start the runtime");
    let snapshot = observable.snapshot().flow;
    assert!(snapshot.enabled);
    assert_eq!(snapshot.peers.len(), 1);
    assert_eq!(snapshot.peers[0].state, FlowLinkState::Lost);

    let peer = FlowEndpoint::bind(
        loopback(),
        remote,
        PeerTrust::pinned([local_key]),
        hello(remote_key),
    )
    .expect("bind the peer");
    let connection = tokio::time::timeout(WAIT, peer.connect(runtime.local_addr()))
        .await
        .expect("connect in time")
        .expect("connect");
    wait_for_link(&observable, FlowLinkState::Connected).await;

    let accept = call(&connection, 41).await;
    assert_eq!(accept.transfer_id, 41);
    assert!(accept.arm_timeout_ms > 0);

    inventory_tx.send_replace(inventory(true));
    let result = next_result(&connection).await;
    assert_eq!(result.transfer_id, 41);
    assert_eq!(result.outcome, proto::HandoffOutcome::Arrived);

    // A retried request replays the result instead of arming again.
    let replay = call(&connection, 41).await;
    assert_eq!(replay.arm_timeout_ms, 0);
    assert_eq!(next_result(&connection).await.outcome, result.outcome);

    runtime.stop().await;
    tokio::time::timeout(WAIT, connection.accept_notification())
        .await
        .expect("the close is seen in time")
        .expect_err("stopping closes the connection");
}

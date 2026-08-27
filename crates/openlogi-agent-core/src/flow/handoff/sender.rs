use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::transport::{FlowConnection, message_envelope};
use openlogi_hook::edge::{EdgeCrossing, EdgeSide};
use tracing::{debug, info, warn};

use super::OutgoingSignal;
use crate::flow::runtime::GenerationState;

const NETWORK_SLACK: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub(in crate::flow) fn start_outgoing(state: Arc<GenerationState>, crossing: EdgeCrossing) {
    if !state.is_active()
        || state
            .handoffs
            .sender_busy
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    tokio::spawn(async move {
        run_outgoing(&state, crossing).await;
        state
            .handoffs
            .sender_busy
            .store(false, std::sync::atomic::Ordering::Release);
    });
}

async fn run_outgoing(state: &Arc<GenerationState>, crossing: EdgeCrossing) {
    // A config reload deactivates a generation at this lock boundary. Once a
    // peer accepts, keep the bounded handoff transaction on one generation
    // through its arrival acknowledgment so stale config cannot switch hosts.
    let _lifecycle = state.lifecycle.lock().await;
    if !state.is_active() {
        return;
    }
    let Some(&peer_index) = state.config.layout.get(&crossing.side) else {
        return;
    };
    let Some(peer) = state.config.peers.get(peer_index) else {
        return;
    };
    let Some(connection) = state.connection(peer.public_key) else {
        debug!(peer = %peer.name, "Flow edge ignored while peer is disconnected");
        return;
    };
    let targets = state.outgoing_devices(&peer.name);
    if targets.is_empty() {
        warn!(peer = %peer.name, "Flow edge has no online configured devices to hand off");
        return;
    }

    let transfer_id = random_transfer_id();
    let mut request = proto::HandoffRequest {
        transfer_id,
        devices: targets
            .iter()
            .map(|target| target.identity.clone())
            .collect(),
        sent_at_ms: unix_millis(),
        ..Default::default()
    };
    *request.entry.get_or_insert_default() = entry_from_crossing(crossing);
    let Ok(envelope) = message_envelope(FrameKind::HandoffRequest, &request) else {
        return;
    };
    // Register before opening the RPC stream: QUIC does not order the response
    // stream against the receiver's HandoffResult notification stream.
    let result = state
        .handoffs
        .register_outgoing(peer.public_key, transfer_id)
        .await;
    let Some(acceptance) = request_acceptance(&connection, &peer.name, transfer_id, envelope).await
    else {
        state.handoffs.remove_outgoing(transfer_id).await;
        return;
    };
    match state.switch_devices(&targets).await {
        Ok(true) => {}
        Ok(false) => {
            state.handoffs.remove_outgoing(transfer_id).await;
            state
                .send_cancel(
                    peer.public_key,
                    transfer_id,
                    proto::HandoffCancelReason::Shutdown,
                )
                .await;
            return;
        }
        Err(error) => {
            state.handoffs.remove_outgoing(transfer_id).await;
            warn!(%error, peer = %peer.name, "Flow host switch failed after receiver acceptance");
            state
                .send_cancel(
                    peer.public_key,
                    transfer_id,
                    proto::HandoffCancelReason::SwitchFailed,
                )
                .await;
            return;
        }
    }

    let deadline = Duration::from_millis(u64::from(acceptance.arm_timeout_ms)) + NETWORK_SLACK;
    match tokio::time::timeout(deadline, result).await {
        Ok(Ok(OutgoingSignal::Result(result))) => {
            info!(
                peer = %peer.name,
                transfer_id,
                outcome = result.outcome.to_i32(),
                "Flow handoff completed"
            );
        }
        Ok(Ok(OutgoingSignal::Cancelled)) => {
            debug!(peer = %peer.name, transfer_id, "Flow handoff cancelled by peer");
        }
        Ok(Err(_)) | Err(_) => {
            state.handoffs.remove_outgoing(transfer_id).await;
            warn!(peer = %peer.name, transfer_id, "Flow arrival acknowledgment timed out");
        }
    }
}

async fn request_acceptance(
    connection: &FlowConnection,
    peer_name: &str,
    transfer_id: u64,
    request: Envelope,
) -> Option<proto::HandoffAccept> {
    let response = match tokio::time::timeout(REQUEST_TIMEOUT, connection.call(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            debug!(%error, peer = peer_name, "Flow handoff request failed");
            return None;
        }
        Err(_) => {
            debug!(peer = peer_name, "Flow handoff request timed out");
            return None;
        }
    };
    if response.kind == FrameKind::HandoffReject {
        if let Ok(rejection) = response.decode::<proto::HandoffReject>(InboundRole::Request) {
            debug!(
                peer = peer_name,
                reason = rejection.reason.to_i32(),
                "Flow handoff rejected"
            );
        }
        return None;
    }
    if response.kind != FrameKind::HandoffAccept {
        return None;
    }
    let Ok(acceptance) = response.decode::<proto::HandoffAccept>(InboundRole::Request) else {
        return None;
    };
    if acceptance.transfer_id != transfer_id || acceptance.arm_timeout_ms == 0 {
        warn!(
            peer = peer_name,
            "Flow peer returned an invalid handoff acceptance"
        );
        return None;
    }
    Some(acceptance)
}

fn entry_from_crossing(crossing: EdgeCrossing) -> proto::EntryPoint {
    let mut entry = proto::EntryPoint {
        side: receiver_side(crossing.side).into(),
        t: crossing.t.clamp(0.0, 1.0),
        ..Default::default()
    };
    *entry.velocity.get_or_insert_default() = proto::Vec2 {
        x: crossing.velocity.x,
        y: crossing.velocity.y,
        ..Default::default()
    };
    entry
}

const fn receiver_side(side: EdgeSide) -> proto::Side {
    match side {
        EdgeSide::Left => proto::Side::Right,
        EdgeSide::Right => proto::Side::Left,
        EdgeSide::Top => proto::Side::Bottom,
        EdgeSide::Bottom => proto::Side::Top,
    }
}

fn random_transfer_id() -> u64 {
    loop {
        let id = rand::random();
        if id != 0 {
            return id;
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

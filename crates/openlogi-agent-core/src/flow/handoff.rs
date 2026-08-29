use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use openlogi_flow::frame::{FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::identity::same_device;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::transport::{IncomingRpc, NotificationEvent, RpcEvent, message_envelope};
use tokio::sync::{Mutex, oneshot};
use tokio::time::Instant;
use tracing::debug;

use super::runtime::GenerationState;
use super::{RuntimeDevice, is_pointing_device};

const ARM_TIMEOUT: Duration = Duration::from_secs(3);
const COMPLETED_LIMIT: usize = 64;

mod sender;

pub(super) use sender::start_outgoing;

#[derive(Default)]
pub(super) struct HandoffBook {
    incoming: Mutex<IncomingLedger>,
    outgoing: Mutex<HashMap<u64, OutgoingWaiter>>,
    sender_busy: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct IncomingLedger {
    pending: Option<PendingIncoming>,
    completed: HashMap<(PublicKey, u64), CompletedIncoming>,
    completion_order: VecDeque<(PublicKey, u64)>,
}

struct PendingIncoming {
    peer: PublicKey,
    transfer_id: u64,
    entry: proto::EntryPoint,
    devices: Vec<PendingDevice>,
    accepted_at: Option<Instant>,
}

struct PendingDevice {
    key: String,
    identity: proto::DeviceIdentity,
    pointing: bool,
    arrived_at: Option<Instant>,
}

#[derive(Clone)]
struct CompletedIncoming {
    accept: proto::HandoffAccept,
    result: proto::HandoffResult,
}

struct OutgoingWaiter {
    peer: PublicKey,
    result: oneshot::Sender<OutgoingSignal>,
}

enum OutgoingSignal {
    Result(proto::HandoffResult),
    Cancelled,
}

enum ArmDecision {
    New(proto::HandoffAccept),
    Duplicate(proto::HandoffAccept),
    Replay {
        accept: proto::HandoffAccept,
        result: proto::HandoffResult,
    },
    Reject(proto::HandoffReject),
}

impl HandoffBook {
    async fn arm(
        &self,
        peer: PublicKey,
        request: &proto::HandoffRequest,
        local_devices: &[RuntimeDevice],
        enabled: bool,
    ) -> ArmDecision {
        let reject = |reason: proto::HandoffRejectReason, detail: &str| {
            ArmDecision::Reject(proto::HandoffReject {
                transfer_id: request.transfer_id,
                reason: reason.into(),
                detail: detail.to_owned(),
                ..Default::default()
            })
        };
        if !enabled {
            return reject(proto::HandoffRejectReason::Disabled, "Flow is disabled");
        }
        if request.transfer_id == 0 || request.devices.is_empty() || !valid_entry(request) {
            return reject(
                proto::HandoffRejectReason::NotReady,
                "invalid handoff request",
            );
        }

        let key = (peer, request.transfer_id);
        let mut ledger = self.incoming.lock().await;
        if let Some(completed) = ledger.completed.get(&key) {
            return ArmDecision::Replay {
                accept: completed.accept.clone(),
                result: completed.result.clone(),
            };
        }
        if let Some(pending) = &ledger.pending {
            if pending.peer == peer && pending.transfer_id == request.transfer_id {
                return ArmDecision::Duplicate(accept(request.transfer_id));
            }
            return reject(
                proto::HandoffRejectReason::AlreadyPending,
                "another transfer is already armed",
            );
        }

        let mut matched = Vec::with_capacity(request.devices.len());
        for requested in &request.devices {
            let local = local_devices
                .iter()
                .find(|local| same_device(requested, &local.identity).unwrap_or(false));
            let Some(local) = local else {
                return reject(
                    proto::HandoffRejectReason::UnknownDevice,
                    "a requested device is not configured locally",
                );
            };
            if local.snapshot.online {
                return reject(
                    proto::HandoffRejectReason::NotReady,
                    "a requested device is already online on the receiver",
                );
            }
            if matched
                .iter()
                .any(|device: &PendingDevice| device.key == local.snapshot.config_key)
            {
                return reject(
                    proto::HandoffRejectReason::UnknownDevice,
                    "the request repeats one physical device",
                );
            }
            matched.push(PendingDevice {
                key: local.snapshot.config_key.clone(),
                identity: local.identity.clone(),
                pointing: is_pointing_device(local.snapshot.kind),
                arrived_at: None,
            });
        }

        ledger.pending = Some(PendingIncoming {
            peer,
            transfer_id: request.transfer_id,
            entry: request.entry.as_option().cloned().unwrap_or_default(),
            devices: matched,
            accepted_at: None,
        });
        ArmDecision::New(accept(request.transfer_id))
    }

    async fn mark_accepted(&self, peer: PublicKey, transfer_id: u64) -> bool {
        let mut ledger = self.incoming.lock().await;
        let Some(pending) = ledger.pending.as_mut() else {
            return false;
        };
        if pending.peer != peer || pending.transfer_id != transfer_id {
            return false;
        }
        if pending.accepted_at.is_some() {
            return false;
        }
        pending.accepted_at = Some(Instant::now());
        true
    }

    async fn observe(&self, devices: &[RuntimeDevice]) -> Option<CompletedTransfer> {
        let mut ledger = self.incoming.lock().await;
        let pending = ledger.pending.as_mut()?;
        for expected in &mut pending.devices {
            if expected.arrived_at.is_none()
                && devices.iter().any(|device| {
                    device.snapshot.config_key == expected.key && device.snapshot.online
                })
            {
                expected.arrived_at = Some(Instant::now());
            }
        }
        let accepted = pending.accepted_at?;
        if pending
            .devices
            .iter()
            .all(|device| device.arrived_at.is_some())
        {
            return Some(complete_pending(&mut ledger, accepted, false));
        }
        None
    }

    async fn expire(&self, peer: PublicKey, transfer_id: u64) -> Option<CompletedTransfer> {
        let mut ledger = self.incoming.lock().await;
        let pending = ledger.pending.as_ref()?;
        if pending.peer != peer || pending.transfer_id != transfer_id {
            return None;
        }
        let accepted = pending.accepted_at?;
        Some(complete_pending(&mut ledger, accepted, true))
    }

    async fn cancel_unaccepted(&self, peer: PublicKey, transfer_id: u64) -> bool {
        let mut ledger = self.incoming.lock().await;
        if ledger.pending.as_ref().is_some_and(|pending| {
            pending.peer == peer
                && pending.transfer_id == transfer_id
                && pending.accepted_at.is_none()
        }) {
            ledger.pending = None;
            true
        } else {
            false
        }
    }

    async fn cancel_incoming(&self, peer: PublicKey, transfer_id: u64) -> bool {
        let mut ledger = self.incoming.lock().await;
        if ledger
            .pending
            .as_ref()
            .is_some_and(|pending| pending.peer == peer && pending.transfer_id == transfer_id)
        {
            ledger.pending = None;
            true
        } else {
            false
        }
    }

    async fn register_outgoing(
        &self,
        peer: PublicKey,
        transfer_id: u64,
    ) -> oneshot::Receiver<OutgoingSignal> {
        let (sender, receiver) = oneshot::channel();
        self.outgoing.lock().await.insert(
            transfer_id,
            OutgoingWaiter {
                peer,
                result: sender,
            },
        );
        receiver
    }

    async fn remove_outgoing(&self, transfer_id: u64) {
        self.outgoing.lock().await.remove(&transfer_id);
    }

    async fn deliver_result(&self, peer: PublicKey, result: proto::HandoffResult) -> bool {
        let mut outgoing = self.outgoing.lock().await;
        let Some(waiter) = outgoing.remove(&result.transfer_id) else {
            return false;
        };
        if waiter.peer != peer {
            outgoing.insert(result.transfer_id, waiter);
            return false;
        }
        let _ = waiter.result.send(OutgoingSignal::Result(result));
        true
    }

    async fn cancel_outgoing(&self, peer: PublicKey, transfer_id: u64) -> bool {
        let mut outgoing = self.outgoing.lock().await;
        let Some(waiter) = outgoing.remove(&transfer_id) else {
            return false;
        };
        if waiter.peer != peer {
            outgoing.insert(transfer_id, waiter);
            return false;
        }
        let _ = waiter.result.send(OutgoingSignal::Cancelled);
        true
    }
}

pub(super) async fn handle_rpc(state: Arc<GenerationState>, peer: PublicKey, event: RpcEvent) {
    let RpcEvent::Request(rpc) = event else {
        return;
    };
    if rpc.request().kind != FrameKind::HandoffRequest {
        return;
    }
    let Ok(request) = rpc
        .request()
        .decode::<proto::HandoffRequest>(InboundRole::Request)
    else {
        return;
    };
    let devices = state
        .devices
        .read()
        .map_or_else(|_| Vec::new(), |guard| guard.clone());
    let decision = state
        .handoffs
        .arm(
            peer,
            &request,
            &devices,
            state.config.enabled && state.is_active(),
        )
        .await;
    respond_to_arm(state, peer, rpc, decision).await;
}

async fn respond_to_arm(
    state: Arc<GenerationState>,
    peer: PublicKey,
    rpc: IncomingRpc,
    decision: ArmDecision,
) {
    match decision {
        ArmDecision::New(acceptance) => {
            let transfer_id = acceptance.transfer_id;
            let Ok(envelope) = message_envelope(FrameKind::HandoffAccept, &acceptance) else {
                return;
            };
            if rpc.respond(envelope).await.is_err() {
                state.handoffs.cancel_unaccepted(peer, transfer_id).await;
                return;
            }
            accept_pending(state, peer, transfer_id).await;
        }
        ArmDecision::Duplicate(acceptance) => {
            let transfer_id = acceptance.transfer_id;
            if let Ok(envelope) = message_envelope(FrameKind::HandoffAccept, &acceptance)
                && rpc.respond(envelope).await.is_ok()
            {
                accept_pending(state, peer, transfer_id).await;
            }
        }
        ArmDecision::Replay { accept, result } => {
            let Ok(envelope) = message_envelope(FrameKind::HandoffAccept, &accept) else {
                return;
            };
            if rpc.respond(envelope).await.is_ok() {
                let _lifecycle = state.lifecycle.lock().await;
                if state.is_active() {
                    state.send_result(peer, result).await;
                }
            }
        }
        ArmDecision::Reject(rejection) => {
            if let Ok(envelope) = message_envelope(FrameKind::HandoffReject, &rejection) {
                let _ = rpc.respond(envelope).await;
            }
        }
    }
}

async fn accept_pending(state: Arc<GenerationState>, peer: PublicKey, transfer_id: u64) {
    let completed = {
        let _lifecycle = state.lifecycle.lock().await;
        if !state.is_active() {
            state.handoffs.cancel_incoming(peer, transfer_id).await;
            state
                .send_cancel(peer, transfer_id, proto::HandoffCancelReason::Shutdown)
                .await;
            return;
        }
        if state.handoffs.mark_accepted(peer, transfer_id).await {
            spawn_arm_timeout(Arc::clone(&state), peer, transfer_id);
            state.handoffs.observe(&state.devices_snapshot()).await
        } else {
            None
        }
    };
    if let Some(completed) = completed {
        finish_incoming(state, completed).await;
    }
}

fn spawn_arm_timeout(state: Arc<GenerationState>, peer: PublicKey, transfer_id: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(ARM_TIMEOUT).await;
        if let Some(completed) = state.handoffs.expire(peer, transfer_id).await {
            finish_incoming(state, completed).await;
        }
    });
}

pub(super) async fn inventory_changed(state: Arc<GenerationState>) {
    if let Some(completed) = state.handoffs.observe(&state.devices_snapshot()).await {
        finish_incoming(state, completed).await;
    }
}

async fn finish_incoming(state: Arc<GenerationState>, completed: CompletedTransfer) {
    let _lifecycle = state.lifecycle.lock().await;
    if !state.is_active() {
        state
            .send_cancel(
                completed.peer,
                completed.result.transfer_id,
                proto::HandoffCancelReason::Shutdown,
            )
            .await;
        return;
    }
    if completed.warp_pointer {
        super::runtime::warp_entry(&completed.entry);
    }
    state.send_result(completed.peer, completed.result).await;
}

pub(super) async fn handle_notification(
    state: Arc<GenerationState>,
    peer: PublicKey,
    event: NotificationEvent,
) {
    let NotificationEvent::Notification(notification) = event else {
        return;
    };
    match notification.kind {
        FrameKind::HandoffResult => {
            let Ok(result) = notification.decode::<proto::HandoffResult>(InboundRole::Notification)
            else {
                return;
            };
            if !state.handoffs.deliver_result(peer, result.clone()).await {
                debug!(
                    transfer_id = result.transfer_id,
                    "stale Flow handoff result ignored"
                );
            }
        }
        FrameKind::HandoffCancel => {
            let Ok(cancel) = notification.decode::<proto::HandoffCancel>(InboundRole::Notification)
            else {
                return;
            };
            let incoming = state
                .handoffs
                .cancel_incoming(peer, cancel.transfer_id)
                .await;
            let outgoing = state
                .handoffs
                .cancel_outgoing(peer, cancel.transfer_id)
                .await;
            if !(incoming || outgoing) {
                debug!(
                    transfer_id = cancel.transfer_id,
                    "stale Flow handoff cancel ignored"
                );
            }
        }
        _ => {}
    }
}

struct CompletedTransfer {
    peer: PublicKey,
    entry: proto::EntryPoint,
    result: proto::HandoffResult,
    warp_pointer: bool,
}

fn complete_pending(
    ledger: &mut IncomingLedger,
    accepted_at: Instant,
    timed_out: bool,
) -> CompletedTransfer {
    let pending = ledger
        .pending
        .take()
        .unwrap_or_else(|| unreachable!("completion requires a pending transfer"));
    let arrived_count = pending
        .devices
        .iter()
        .filter(|device| device.arrived_at.is_some())
        .count();
    let outcome = if arrived_count == pending.devices.len() {
        proto::HandoffOutcome::Arrived
    } else if arrived_count > 0 {
        proto::HandoffOutcome::Partial
    } else if timed_out {
        proto::HandoffOutcome::Timeout
    } else {
        unreachable!("non-timeout completion has at least one arrival")
    };
    let arrivals = pending
        .devices
        .iter()
        .map(|device| {
            let mut arrival = proto::DeviceArrival {
                arrived: device.arrived_at.is_some(),
                elapsed_ms: device.arrived_at.map_or(0, |arrived| {
                    u32::try_from(arrived.duration_since(accepted_at).as_millis())
                        .unwrap_or(u32::MAX)
                }),
                ..Default::default()
            };
            *arrival.device.get_or_insert_default() = device.identity.clone();
            arrival
        })
        .collect();
    let result = proto::HandoffResult {
        transfer_id: pending.transfer_id,
        outcome: outcome.into(),
        arrivals,
        ..Default::default()
    };
    let completed = CompletedIncoming {
        accept: accept(pending.transfer_id),
        result: result.clone(),
    };
    let key = (pending.peer, pending.transfer_id);
    ledger.completed.insert(key, completed);
    ledger.completion_order.push_back(key);
    while ledger.completion_order.len() > COMPLETED_LIMIT {
        if let Some(expired) = ledger.completion_order.pop_front() {
            ledger.completed.remove(&expired);
        }
    }
    CompletedTransfer {
        peer: pending.peer,
        entry: pending.entry,
        result,
        warp_pointer: pending
            .devices
            .iter()
            .any(|device| device.pointing && device.arrived_at.is_some()),
    }
}

fn accept(transfer_id: u64) -> proto::HandoffAccept {
    proto::HandoffAccept {
        transfer_id,
        arm_timeout_ms: u32::try_from(ARM_TIMEOUT.as_millis())
            .unwrap_or_else(|_| unreachable!("three seconds fits in u32 milliseconds")),
        ..Default::default()
    }
}

fn valid_entry(request: &proto::HandoffRequest) -> bool {
    request.entry.as_option().is_some_and(|entry| {
        entry
            .side
            .as_known()
            .is_some_and(|side| side != proto::Side::Unspecified)
            && entry.t.is_finite()
            && (0.0..=1.0).contains(&entry.t)
    })
}

#[cfg(test)]
mod tests;

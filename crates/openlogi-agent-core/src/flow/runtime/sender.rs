//! Handing this host's devices to a peer.
//!
//! One task runs one outgoing transfer in two phases. The hardware phase
//! takes the exclusive receiver lease within [`LEASE_BUDGET`] and prepares
//! each device's switch, so every read happens before the peer hears of the
//! transfer. The negotiation phase sends the request, writes the prepared
//! switches while the accept's write window is open and device I/O is
//! allowed, releases the lease, and then either cancels the transfer (no
//! write started) or waits for the peer's result. Every step has a deadline.

use std::future::Future;
use std::hash::{BuildHasher as _, RandomState};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use openlogi_flow::frame::{FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::transport::{FlowConnection, TransportError, message_envelope};
use openlogi_hid::{
    ChannelPool, DeviceIoGate, HostSwitchError, PreparedHostSwitch, prepare_host_switch,
};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use super::link::NOTIFY_TIMEOUT;
use super::unix_millis;
use crate::flow::devices::OutgoingDevice;
use crate::flow::handoff::{
    AfterSwitch, NETWORK_SLACK, SwitchOutcome, SwitchSchedule, after_switch,
};
use crate::receiver_access::{ExclusiveAccessReason, ExclusiveReceiverLease, ReceiverAccess};

#[cfg(test)]
mod tests;

/// Longest wait for the exclusive receiver lease.
const LEASE_BUDGET: Duration = Duration::from_secs(2);
/// Longest preparation of one device's switch.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest wait for the peer's answer to a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for one switch write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// The agent's device access a sender needs.
#[derive(Clone)]
pub(crate) struct FlowHardware {
    /// Opens the device channels.
    pub(crate) channel_pool: ChannelPool,
    /// Orders device I/O with the other receiver users.
    pub(crate) receiver_access: ReceiverAccess,
    /// Closed while the agent suspends device I/O.
    pub(crate) device_io: DeviceIoGate,
}

/// How an outgoing transfer ended.
#[derive(Debug, PartialEq)]
pub(super) enum SendReport {
    /// Nothing reached the peer or a device.
    NotStarted(&'static str),
    /// The peer refused the transfer.
    Rejected(proto::HandoffRejectReason),
    /// No write started; the peer was told to disarm.
    Cancelled(proto::HandoffCancelReason),
    /// The peer reported the outcome.
    Finished(proto::HandoffResult),
    /// A write started, and no result arrived before the deadline or the
    /// runtime stopped.
    NoResult,
}

/// One prepared write. Implemented by [`PreparedHostSwitch`]; tests script it.
pub(super) trait Switch: Send {
    /// Issue the write.
    fn apply(self) -> impl Future<Output = Result<bool, HostSwitchError>> + Send;
}

impl Switch for PreparedHostSwitch {
    fn apply(self) -> impl Future<Output = Result<bool, HostSwitchError>> + Send {
        PreparedHostSwitch::apply(self)
    }
}

/// The parts of one transfer the negotiation phase needs.
pub(super) struct Negotiation<S> {
    pub(super) connection: Arc<FlowConnection>,
    pub(super) transfer_id: u64,
    pub(super) entry: proto::EntryPoint,
    /// The devices in request order, each with its prepared write.
    pub(super) switches: Vec<(proto::DeviceIdentity, S)>,
    /// Fed by the actor when the peer's result arrives.
    pub(super) result: oneshot::Receiver<proto::HandoffResult>,
    pub(super) stop: watch::Receiver<bool>,
    /// Closed while the agent suspends device I/O.
    pub(super) device_io: DeviceIoGate,
}

/// Take the lease and prepare the switches. The lease is held until the
/// writes are done.
pub(super) async fn acquire_and_prepare(
    hardware: &FlowHardware,
    devices: &[OutgoingDevice],
) -> Result<
    (
        ExclusiveReceiverLease,
        Vec<(proto::DeviceIdentity, PreparedHostSwitch)>,
    ),
    SendReport,
> {
    let lease = acquire(&hardware.receiver_access)
        .await
        .ok_or(SendReport::NotStarted("the receivers stayed busy"))?;
    if !hardware.device_io.allows_io() {
        return Err(SendReport::NotStarted("device I/O is suspended"));
    }
    let switches = prepare_all(devices, |device| async move {
        let switch =
            prepare_host_switch(&hardware.channel_pool, &device.route, device.slot.get()).await?;
        Ok(switch.is_required().then_some(switch))
    })
    .await?;
    Ok((lease, switches))
}

/// Prepare every device's switch, or none. `prepare` answers `None` for a
/// device already on the peer's slot.
///
/// A device that cannot switch keeps the whole transfer here: the request
/// would list only the others, so the peer would report a complete transfer
/// while a keyboard stayed behind its mouse.
async fn prepare_all<'a, S, Prepare, Prepared>(
    devices: &'a [OutgoingDevice],
    prepare: Prepare,
) -> Result<Vec<(proto::DeviceIdentity, S)>, SendReport>
where
    Prepare: Fn(&'a OutgoingDevice) -> Prepared,
    Prepared: Future<Output = Result<Option<S>, HostSwitchError>>,
{
    let mut switches = Vec::with_capacity(devices.len());
    for device in devices {
        match tokio::time::timeout(PREPARE_TIMEOUT, prepare(device)).await {
            Ok(Ok(Some(switch))) => switches.push((device.identity.clone(), switch)),
            Ok(Ok(None)) => debug!(route = %device.route, "device already on the peer's slot"),
            Ok(Err(error)) => {
                warn!(%error, route = %device.route, "cannot prepare the Flow switch; no device moves");
                return Err(SendReport::NotStarted("a device could not be prepared"));
            }
            Err(_) => {
                warn!(route = %device.route, "preparing the Flow switch timed out; no device moves");
                return Err(SendReport::NotStarted("a device could not be prepared"));
            }
        }
    }
    if switches.is_empty() {
        return Err(SendReport::NotStarted("every device is on the peer's slot"));
    }
    Ok(switches)
}

async fn acquire(access: &ReceiverAccess) -> Option<ExclusiveReceiverLease> {
    tokio::time::timeout(
        LEASE_BUDGET,
        access.acquire_exclusive(ExclusiveAccessReason::HostTransition),
    )
    .await
    .ok()
}

/// Request the transfer, write the switches, and settle it. `lease` is
/// released as soon as the writes end.
pub(super) async fn negotiate<S: Switch>(
    negotiation: Negotiation<S>,
    lease: impl Send,
) -> SendReport {
    let Negotiation {
        connection,
        transfer_id,
        entry,
        switches,
        result,
        mut stop,
        device_io,
    } = negotiation;
    let mut request = proto::HandoffRequest {
        transfer_id,
        devices: switches
            .iter()
            .map(|(identity, _)| identity.clone())
            .collect(),
        sent_at_ms: unix_millis(),
        ..Default::default()
    };
    *request.entry.get_or_insert_default() = entry;
    let requested_at = Instant::now();
    let accept = match call(&connection, &request).await {
        Ok(Answer::Accept(accept)) if accept.transfer_id == transfer_id => accept,
        Ok(Answer::Reject(reason)) => return SendReport::Rejected(reason),
        Ok(Answer::Accept(_)) | Err(()) => {
            // The peer may have armed; no write started.
            let reason = proto::HandoffCancelReason::SwitchFailed;
            cancel(&connection, transfer_id, reason).await;
            return SendReport::Cancelled(reason);
        }
    };
    let accepted_at = Instant::now();
    if accept.arm_timeout_ms == 0 {
        // A replay: the peer finished this transfer already and resent the
        // result.
        drop(lease);
        return await_result(result, accepted_at + NETWORK_SLACK, stop).await;
    }

    let schedule = SwitchSchedule::new(requested_at, accepted_at, &accept);
    let mut outcomes = Vec::with_capacity(switches.len());
    let mut stopping = false;
    for (_, switch) in switches {
        stopping = stopping || *stop.borrow_and_update();
        // The request's round trip can outlast an open gate. The transport
        // would refuse the write, but a refused write counts as started and
        // leaves the peer waiting out its window instead of disarming.
        if stopping || !device_io.allows_io() || !schedule.may_write(Instant::now()) {
            outcomes.push(SwitchOutcome::NotWritten);
            continue;
        }
        outcomes.push(
            match tokio::time::timeout(WRITE_TIMEOUT, switch.apply()).await {
                Ok(Ok(_)) => SwitchOutcome::Written,
                Ok(Err(error)) => {
                    warn!(%error, transfer = transfer_id, "Flow switch write failed");
                    SwitchOutcome::Failed
                }
                Err(_) => {
                    warn!(transfer = transfer_id, "Flow switch write timed out");
                    SwitchOutcome::Failed
                }
            },
        );
    }
    drop(lease);

    match after_switch(&outcomes, stopping) {
        AfterSwitch::Cancel(reason) => {
            cancel(&connection, transfer_id, reason).await;
            SendReport::Cancelled(reason)
        }
        // Shutting down: nobody is left to act on the result.
        AfterSwitch::AwaitResult if stopping => SendReport::NoResult,
        AfterSwitch::AwaitResult => await_result(result, schedule.give_up(), stop).await,
    }
}

enum Answer {
    Accept(proto::HandoffAccept),
    Reject(proto::HandoffRejectReason),
}

async fn call(connection: &FlowConnection, request: &proto::HandoffRequest) -> Result<Answer, ()> {
    let envelope = message_envelope(FrameKind::HandoffRequest, request).map_err(|error| {
        warn!(%error, "could not encode a Flow handoff request");
    })?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, connection.call(envelope))
        .await
        .unwrap_or(Err(TransportError::StreamTimeout))
        .map_err(|error| debug!(%error, "Flow handoff request failed"))?;
    match response.kind {
        FrameKind::HandoffAccept => response
            .decode::<proto::HandoffAccept>(InboundRole::Notification)
            .map(Answer::Accept)
            .map_err(|_| debug!("malformed Flow handoff accept")),
        FrameKind::HandoffReject => {
            let reject = response
                .decode::<proto::HandoffReject>(InboundRole::Notification)
                .map_err(|_| debug!("malformed Flow handoff reject"))?;
            let reason = reject
                .reason
                .as_known()
                .unwrap_or(proto::HandoffRejectReason::Unspecified);
            Ok(Answer::Reject(reason))
        }
        kind => {
            debug!(?kind, "unexpected answer to a Flow handoff request");
            Err(())
        }
    }
}

async fn cancel(connection: &FlowConnection, transfer_id: u64, reason: proto::HandoffCancelReason) {
    let cancel = proto::HandoffCancel {
        transfer_id,
        reason: reason.into(),
        ..Default::default()
    };
    let delivered = match message_envelope(FrameKind::HandoffCancel, &cancel) {
        Ok(envelope) => tokio::time::timeout(NOTIFY_TIMEOUT, connection.notify(envelope))
            .await
            .unwrap_or(Err(TransportError::StreamTimeout)),
        Err(error) => Err(error),
    };
    if let Err(error) = delivered {
        debug!(%error, transfer = transfer_id, "Flow handoff cancel not delivered");
    }
}

async fn await_result(
    result: oneshot::Receiver<proto::HandoffResult>,
    give_up: Instant,
    mut stop: watch::Receiver<bool>,
) -> SendReport {
    tokio::select! {
        biased;
        _ = stop.wait_for(|stopping| *stopping) => SendReport::NoResult,
        result = result => result.map_or(SendReport::NoResult, SendReport::Finished),
        () = tokio::time::sleep_until(give_up) => SendReport::NoResult,
    }
}

/// A fresh transfer id. Ids must be unique, not secret: `RandomState` keys
/// are random per process, and each id hashes a counter and the clock.
pub(super) fn new_transfer_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let id = RandomState::new()
            .hash_one((COUNTER.fetch_add(1, Ordering::Relaxed), SystemTime::now()));
        if id != 0 {
            return id;
        }
    }
}

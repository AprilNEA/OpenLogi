//! Handoff transaction state, owned by the Flow runtime actor.
//!
//! Nothing here performs I/O or reads the clock. The actor passes the current
//! [`Instant`] in, sends what these types decide, and drives the timers they
//! report, so every deadline rule is testable without a network or HID.
//!
//! Receiver rules:
//! - The arrival window starts when the receiver decides to accept. That is
//!   no later than the moment the sender sees the accept, so the receiver
//!   never watches for less time than the sender assumes.
//! - Losing the connection, or failing to send the accept, does not drop the
//!   transfer. The accept may still have reached the sender, so the transfer
//!   runs to its deadline.
//! - A cancel disarms the transfer only while no device has arrived. After an
//!   arrival the moved devices are usable here, so the transfer completes and
//!   reports what arrived.
//! - The cursor is placed when the first pointing device arrives, not when
//!   the whole transfer completes.
//!
//! Sender rules:
//! - Every switch write must start before [`SwitchSchedule::may_write`] turns
//!   false: half the receiver's window after the request was sent, so the
//!   device lands while the receiver still watches. The receiver's window may
//!   open as soon as the request arrives, however long its accept takes.
//! - Once any write was attempted the sender must not cancel. The device may
//!   already be on its way, and the receiver reports the outcome.
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use tokio::time::Instant;

use super::devices::LocalDevices;
use super::edge::{EdgeCrossing, EdgeSide};

/// How long this receiver watches for arrival after accepting.
pub(crate) const ARM_TIMEOUT: Duration = Duration::from_secs(3);
/// The longest arrival window a sender honors from a peer's accept.
pub(crate) const MAX_ARM_TIMEOUT: Duration = Duration::from_secs(10);
/// The time a sender adds to the receiver's window before it stops waiting
/// for the result.
pub(crate) const NETWORK_SLACK: Duration = Duration::from_secs(2);
/// How many finished incoming transfers are remembered for retransmissions.
const COMPLETED_LIMIT: usize = 64;

/// The receiver's record of incoming transfers. At most one is armed.
#[derive(Debug, Default)]
pub(crate) struct IncomingLedger {
    pending: Option<PendingIncoming>,
    completed: HashMap<(PublicKey, u64), CompletedIncoming>,
    completion_order: VecDeque<(PublicKey, u64)>,
}

#[derive(Debug)]
struct PendingIncoming {
    peer: PublicKey,
    transfer_id: u64,
    entry: proto::EntryPoint,
    devices: Vec<PendingDevice>,
    accepted_at: Instant,
    deadline: Instant,
    entry_placed: bool,
}

#[derive(Debug)]
struct PendingDevice {
    key: PhysicalDeviceKey,
    identity: proto::DeviceIdentity,
    arrived_after: Option<Duration>,
}

#[derive(Clone, Debug)]
struct CompletedIncoming {
    accept: proto::HandoffAccept,
    result: proto::HandoffResult,
}

/// How to answer one `HandoffRequest`.
#[derive(Debug, PartialEq)]
pub(crate) enum ArmDecision {
    /// A new transfer is armed. Answer with the accept.
    Armed(proto::HandoffAccept),
    /// A retransmission of the armed transfer. Answer with the accept again;
    /// its window is the time left.
    Duplicate(proto::HandoffAccept),
    /// A retransmission of a finished transfer. Answer with the accept, then
    /// send the result again. The accept carries a zero window, so a sender
    /// that only now sees it issues no write.
    Replay {
        /// The response to the request.
        accept: proto::HandoffAccept,
        /// The result to send again.
        result: proto::HandoffResult,
    },
    /// Refuse the request.
    Rejected(proto::HandoffReject),
}

/// A finished incoming transfer whose result goes to `peer`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompletedTransfer {
    /// The sender.
    pub(crate) peer: PublicKey,
    /// The result to send.
    pub(crate) result: proto::HandoffResult,
}

/// What one inventory update changed for the armed transfer.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Observation {
    /// The first pointing device just arrived: place the cursor here.
    pub(crate) entry: Option<proto::EntryPoint>,
    /// Every device arrived, so the transfer finished.
    pub(crate) completed: Option<CompletedTransfer>,
}

/// What one `HandoffCancel` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    /// No device had arrived, so the transfer is disarmed.
    Disarmed,
    /// A device already arrived, so the transfer completes on its own.
    Ignored,
    /// The cancel names no armed transfer from that peer.
    Unknown,
}

impl IncomingLedger {
    /// Decide how to answer a request from `peer`, arming a new transfer when
    /// the request is acceptable. `now` is the decision time and starts the
    /// arrival window.
    pub(crate) fn arm(
        &mut self,
        peer: PublicKey,
        request: &proto::HandoffRequest,
        devices: &LocalDevices,
        enabled: bool,
        now: Instant,
    ) -> ArmDecision {
        let transfer_id = request.transfer_id;
        let rejected =
            |reason, detail: &str| ArmDecision::Rejected(reject(transfer_id, reason, detail));
        if !enabled {
            return rejected(proto::HandoffRejectReason::Disabled, "Flow is disabled");
        }
        let Some(entry) =
            valid_entry(request).filter(|_| transfer_id != 0 && !request.devices.is_empty())
        else {
            return rejected(
                proto::HandoffRejectReason::NotReady,
                "invalid handoff request",
            );
        };

        if let Some(done) = self.completed.get(&(peer, transfer_id)) {
            return ArmDecision::Replay {
                accept: done.accept.clone(),
                result: done.result.clone(),
            };
        }
        if let Some(pending) = &self.pending {
            if pending.peer == peer && pending.transfer_id == transfer_id {
                let left = pending.deadline.saturating_duration_since(now);
                return ArmDecision::Duplicate(accept(transfer_id, left));
            }
            return rejected(
                proto::HandoffRejectReason::AlreadyPending,
                "another transfer is already armed",
            );
        }

        let mut matched: Vec<PendingDevice> = Vec::with_capacity(request.devices.len());
        for requested in &request.devices {
            let (key, identity) = if let Some(local) = devices.find(requested) {
                if local.is_online() {
                    return rejected(
                        proto::HandoffRejectReason::NotReady,
                        "a requested device is already connected here",
                    );
                }
                (local.key(), local.identity())
            } else if let Some(key) = devices.find_unlisted(requested) {
                (key, requested)
            } else {
                return rejected(
                    proto::HandoffRejectReason::UnknownDevice,
                    "a requested device is not configured here",
                );
            };
            if matched.iter().any(|device| device.key == *key) {
                return rejected(
                    proto::HandoffRejectReason::UnknownDevice,
                    "the request names one device twice",
                );
            }
            matched.push(PendingDevice {
                key: key.clone(),
                identity: identity.clone(),
                arrived_after: None,
            });
        }

        self.pending = Some(PendingIncoming {
            peer,
            transfer_id,
            entry,
            devices: matched,
            accepted_at: now,
            deadline: now + ARM_TIMEOUT,
            entry_placed: false,
        });
        ArmDecision::Armed(accept(transfer_id, ARM_TIMEOUT))
    }

    /// When the armed transfer expires, if one is armed.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    /// Record the devices that are online in a new inventory view.
    pub(crate) fn observe(&mut self, devices: &LocalDevices, now: Instant) -> Observation {
        let Some(pending) = self.pending.as_mut() else {
            return Observation::default();
        };
        let mut entry = None;
        for expected in &mut pending.devices {
            if expected.arrived_after.is_some() {
                continue;
            }
            let Some(arrived) = devices
                .iter()
                .find(|device| *device.key() == expected.key && device.is_online())
            else {
                continue;
            };
            expected.arrived_after = Some(now.saturating_duration_since(pending.accepted_at));
            // The kind of a device armed before the inventory listed it is
            // known only now.
            if arrived.is_pointing() && !pending.entry_placed {
                pending.entry_placed = true;
                entry = Some(pending.entry.clone());
            }
        }
        let all_arrived = pending
            .devices
            .iter()
            .all(|device| device.arrived_after.is_some());
        let completed = if all_arrived {
            self.pending
                .take()
                .map(|pending| self.record(pending, proto::HandoffOutcome::Arrived))
        } else {
            None
        };
        Observation { entry, completed }
    }

    /// Finish the armed transfer when its window has closed.
    pub(crate) fn expire(&mut self, now: Instant) -> Option<CompletedTransfer> {
        let pending = self.pending.as_ref()?;
        if now < pending.deadline {
            return None;
        }
        let outcome = if pending
            .devices
            .iter()
            .any(|device| device.arrived_after.is_some())
        {
            proto::HandoffOutcome::Partial
        } else {
            proto::HandoffOutcome::Timeout
        };
        self.pending
            .take()
            .map(|pending| self.record(pending, outcome))
    }

    /// Apply a `HandoffCancel` from `peer`.
    pub(crate) fn cancel(&mut self, peer: PublicKey, transfer_id: u64) -> CancelOutcome {
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| pending.peer == peer && pending.transfer_id == transfer_id)
        else {
            return CancelOutcome::Unknown;
        };
        if pending
            .devices
            .iter()
            .any(|device| device.arrived_after.is_some())
        {
            return CancelOutcome::Ignored;
        }
        if let Some(pending) = self.pending.take() {
            self.record(pending, proto::HandoffOutcome::Cancelled);
        }
        CancelOutcome::Disarmed
    }

    /// Close `pending`, taken from the ledger, with `outcome` and remember
    /// its result.
    fn record(
        &mut self,
        pending: PendingIncoming,
        outcome: proto::HandoffOutcome,
    ) -> CompletedTransfer {
        let result = proto::HandoffResult {
            transfer_id: pending.transfer_id,
            outcome: outcome.into(),
            arrivals: pending
                .devices
                .into_iter()
                .map(|device| {
                    let mut arrival = proto::DeviceArrival {
                        arrived: device.arrived_after.is_some(),
                        elapsed_ms: device.arrived_after.map_or(0, millis),
                        ..Default::default()
                    };
                    *arrival.device.get_or_insert_default() = device.identity;
                    arrival
                })
                .collect(),
            ..Default::default()
        };
        let key = (pending.peer, pending.transfer_id);
        self.completed.insert(
            key,
            CompletedIncoming {
                accept: accept(pending.transfer_id, Duration::ZERO),
                result: result.clone(),
            },
        );
        self.completion_order.push_back(key);
        if self.completion_order.len() > COMPLETED_LIMIT
            && let Some(oldest) = self.completion_order.pop_front()
        {
            self.completed.remove(&oldest);
        }
        CompletedTransfer {
            peer: pending.peer,
            result,
        }
    }
}

/// The sender's deadlines for one accepted transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwitchSchedule {
    write_deadline: Instant,
    give_up: Instant,
}

impl SwitchSchedule {
    /// The deadlines that follow from `accept`, for a request sent at
    /// `requested_at` and accepted at `accepted_at`. The peer's window is
    /// capped at [`MAX_ARM_TIMEOUT`].
    ///
    /// The peer opened its window somewhere between the two instants. Writes
    /// count from the earliest, so a slow accept shortens the write window
    /// instead of moving it past the peer's; giving up counts from the
    /// latest.
    pub(crate) fn new(
        requested_at: Instant,
        accepted_at: Instant,
        accept: &proto::HandoffAccept,
    ) -> Self {
        let window = Duration::from_millis(u64::from(accept.arm_timeout_ms)).min(MAX_ARM_TIMEOUT);
        Self {
            write_deadline: requested_at + window / 2,
            give_up: accepted_at + window + NETWORK_SLACK,
        }
    }

    /// Whether a switch write may still start at `now`.
    pub(crate) fn may_write(&self, now: Instant) -> bool {
        now < self.write_deadline
    }

    /// When the sender stops waiting for the result.
    pub(crate) const fn give_up(&self) -> Instant {
        self.give_up
    }
}

/// What happened to one device's switch write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwitchOutcome {
    /// The write never started.
    NotWritten,
    /// The write completed.
    Written,
    /// The write started and failed. A device that switches hosts may leave
    /// before it acknowledges, so the write may still have taken effect.
    Failed,
}

/// What the sender owes the receiver after its switch writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterSwitch {
    /// No write started. Cancel the transfer with this reason.
    Cancel(proto::HandoffCancelReason),
    /// A write started. Wait for the receiver's result.
    AwaitResult,
}

/// Decide what follows a set of switch writes. `stopping` says the writes
/// stopped because the runtime is shutting down.
pub(crate) fn after_switch(outcomes: &[SwitchOutcome], stopping: bool) -> AfterSwitch {
    if outcomes
        .iter()
        .any(|outcome| *outcome != SwitchOutcome::NotWritten)
    {
        AfterSwitch::AwaitResult
    } else if stopping {
        AfterSwitch::Cancel(proto::HandoffCancelReason::Shutdown)
    } else {
        AfterSwitch::Cancel(proto::HandoffCancelReason::SwitchFailed)
    }
}

/// The entry point a crossing of this host's edge gives the receiver. The
/// receiver is entered through the side opposite the one the cursor left by.
pub(crate) fn entry_point(crossing: &EdgeCrossing) -> proto::EntryPoint {
    let side = match crossing.side {
        EdgeSide::Left => proto::Side::Right,
        EdgeSide::Right => proto::Side::Left,
        EdgeSide::Top => proto::Side::Bottom,
        EdgeSide::Bottom => proto::Side::Top,
    };
    let mut entry = proto::EntryPoint {
        side: side.into(),
        t: crossing.t,
        ..Default::default()
    };
    let velocity = entry.velocity.get_or_insert_default();
    velocity.x = crossing.velocity.x;
    velocity.y = crossing.velocity.y;
    entry
}

fn valid_entry(request: &proto::HandoffRequest) -> Option<proto::EntryPoint> {
    request
        .entry
        .as_option()
        .filter(|entry| {
            entry
                .side
                .as_known()
                .is_some_and(|side| side != proto::Side::Unspecified)
                && entry.t.is_finite()
                && (0.0..=1.0).contains(&entry.t)
        })
        .cloned()
}

fn accept(transfer_id: u64, window: Duration) -> proto::HandoffAccept {
    proto::HandoffAccept {
        transfer_id,
        arm_timeout_ms: millis(window),
        ..Default::default()
    }
}

fn reject(
    transfer_id: u64,
    reason: proto::HandoffRejectReason,
    detail: &str,
) -> proto::HandoffReject {
    proto::HandoffReject {
        transfer_id,
        reason: reason.into(),
        detail: detail.to_owned(),
        ..Default::default()
    }
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;

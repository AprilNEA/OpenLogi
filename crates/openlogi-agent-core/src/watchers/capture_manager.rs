//! The select loop the gesture and keyboard capture managers share.
//!
//! The two managers deliberately keep separate state machines (see
//! [`super::capture_session`]): how many slots they track, how they lease the
//! receiver, and what retiring a session cancels all differ. What they wait
//! on, and in which order, does not. [`run`] is that loop; a
//! [`CaptureManager`] is the state machine it drives.

use openlogi_hid::{DeviceIoGate, PendingCaptureRestore};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use super::retry::wait_for_deadline;
use super::shutdown::ManagerCompletion;
use crate::receiver_access::ReceiverRequestState;

/// Firmware ownership a finished session could not release, and when its
/// manager may next try.
pub(super) struct PendingRestore {
    pub(super) token: PendingCaptureRestore,
    pub(super) retry_at: Instant,
}

/// One capture manager's state machine, as [`run`] drives it.
pub(super) trait CaptureManager {
    /// What the orchestrator publishes for this manager to keep armed.
    type Published: Clone;
    /// An ordered report from one of the manager's session tasks.
    type Event;

    /// Bring the tracked sessions in line with `published`. Only called while
    /// host device I/O is allowed.
    async fn reconcile(&mut self, requests: ReceiverRequestState, published: &Self::Published);

    /// Apply one session report. Returns whether it calls for a reconcile.
    fn handle_session_event(
        &mut self,
        event: Self::Event,
        device_io_allowed: bool,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        published: &watch::Receiver<Self::Published>,
    ) -> bool;

    /// The next instant recovery work can make progress, if any.
    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant>;

    /// Whether any finished session still owes the firmware a restore.
    fn has_pending_restores(&self) -> bool;

    /// Make every owed restore due now.
    fn expedite_pending_restores(&mut self);

    /// Stop every tracked session, wait for each ordered completion, then
    /// retain and retry owed restores until firmware ownership is released.
    async fn drain_for_shutdown(
        &mut self,
        events: &mut mpsc::UnboundedReceiver<Self::Event>,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        published: &watch::Receiver<Self::Published>,
    );
}

/// Everything [`run`] waits on.
pub(super) struct ManagerInputs<M: CaptureManager> {
    /// The orchestrator's publication for this manager.
    pub(super) published: watch::Receiver<M::Published>,
    /// Exclusive receiver requests, which suspend capture while pairing or a
    /// host transition owns the receiver.
    pub(super) receiver_requests: watch::Receiver<ReceiverRequestState>,
    /// Inventory channel publications: a change can unblock an owed restore.
    pub(super) registry_changes: watch::Receiver<()>,
    /// Host device-I/O gate.
    pub(super) device_io: DeviceIoGate,
    /// Reports from the manager's session tasks.
    pub(super) events: mpsc::UnboundedReceiver<M::Event>,
    /// The process lifecycle's stop request.
    pub(super) shutdown: oneshot::Receiver<()>,
}

/// A registry change matters only while a restore is owed; otherwise this
/// never resolves. Returns whether the registry is still open.
async fn wait_for_registry_change(
    changes: &mut watch::Receiver<()>,
    has_pending_restore: bool,
) -> bool {
    if !has_pending_restore {
        return std::future::pending().await;
    }
    changes.changed().await.is_ok()
}

/// Drive `manager` until the lifecycle asks it to stop or one of its
/// control-plane sources closes. Runs for the lifetime of the process.
pub(super) async fn run<M: CaptureManager>(
    mut manager: M,
    inputs: ManagerInputs<M>,
) -> ManagerCompletion {
    let ManagerInputs {
        mut published,
        mut receiver_requests,
        mut registry_changes,
        mut device_io,
        mut events,
        mut shutdown,
    } = inputs;
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            if device_io.allows_io() {
                let requests = *receiver_requests.borrow_and_update();
                let wanted = published.borrow_and_update().clone();
                manager.reconcile(requests, &wanted).await;
            }
        }

        let requests = *receiver_requests.borrow();
        let deadline = manager.deadline(requests, device_io.allows_io());
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            reconcile = true;
            continue;
        }
        let has_pending_restores = manager.has_pending_restores();

        tokio::select! {
            biased;

            _ = &mut shutdown => {
                manager
                    .drain_for_shutdown(&mut events, &receiver_requests, &published)
                    .await;
                return ManagerCompletion::Graceful;
            }
            Some(event) = events.recv() => {
                reconcile |= manager.handle_session_event(
                    event,
                    device_io.allows_io(),
                    &receiver_requests,
                    &published,
                );
            }
            result = published.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return ManagerCompletion::Unexpected,
            },
            result = receiver_requests.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return ManagerCompletion::Unexpected,
            },
            allowed = device_io.changed() => match allowed {
                Some(true) => reconcile = true,
                Some(false) => {}
                None => return ManagerCompletion::Unexpected,
            },
            open = wait_for_registry_change(&mut registry_changes, has_pending_restores) => {
                if !open {
                    return ManagerCompletion::Unexpected;
                }
                if device_io.allows_io() {
                    manager.expedite_pending_restores();
                    reconcile = true;
                }
            }
            () = wait_for_deadline(deadline) => {
                reconcile = true;
            }
        }
    }
}

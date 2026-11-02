//! Native ownership for complete Disable Keys writes.
//!
//! A requester can time out or cancel after the operating system has accepted
//! a report. The worker retains its ordering guards through that native write,
//! so a successor cannot enter the transport until the first write completes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use openlogi_hid::{DisableKeysMask, DisableKeysState, HidppOperation, WriteError};
use tokio::sync::{Notify, oneshot};
use tracing::{debug, warn};

use super::write_order::{PendingWrite, ReservedTurn, WriteTicket};
use super::{DeviceOp, WRITE_TIMEOUT};

/// Persistent cancellation state with a wake-up for pre-I/O waits.
#[derive(Default)]
struct Cancellation {
    requested: AtomicBool,
    changed: Notify,
}

impl Cancellation {
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        let changed = self.changed.notified();
        let mut changed = std::pin::pin!(changed);
        // Register before checking the persistent flag, including cancellation
        // before the worker's runtime first polls this future.
        changed.as_mut().enable();
        if !self.is_requested() {
            changed.await;
        }
    }
}

/// A requester-owned cancellation signal for an owned native write.
struct RequesterCancellation {
    cancelled: Arc<Cancellation>,
    armed: bool,
}

impl RequesterCancellation {
    fn new(cancelled: Arc<Cancellation>) -> Self {
        Self {
            cancelled,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RequesterCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.request();
        }
    }
}

/// Native completion is not requester acceptance. An unconsumed message owns
/// the pending intent so cancelling a buffered success still requests recovery.
struct Completion {
    outcome: Result<DisableKeysState, WriteError>,
    pending: Option<PendingWrite<Option<DisableKeysMask>>>,
}

impl Completion {
    fn accept(self) -> Result<DisableKeysState, WriteError> {
        let state = self.outcome?;
        if self.pending.is_some_and(|pending| !pending.confirm()) {
            return Err(WriteError::WriteSuperseded {
                operation: HidppOperation::WriteDisableKeys,
            });
        }
        Ok(state)
    }
}

/// A Disable Keys worker that owns the setting and transport turns through a
/// native HID write. Dropping the requester handle never aborts device I/O
/// already in progress.
#[must_use = "wait for Disable Keys or explicitly detach the completion owner"]
pub(crate) struct DisableKeysJob {
    cancellation: RequesterCancellation,
    started: oneshot::Receiver<()>,
    result: oneshot::Receiver<Completion>,
    worker: JoinHandle<()>,
}

impl DisableKeysJob {
    /// Wait for the requester deadline without timing out the worker's native
    /// write. An elapsed wait drops this handle, while the worker keeps its
    /// receiver lease and turns until the device operation completes.
    pub(crate) async fn wait(mut self) -> Result<DisableKeysState, WriteError> {
        // Queue and lease waits do not spend the device response budget. A
        // closed start signal means the owner returned without starting I/O;
        // its typed result below still reports the reason.
        let _ = (&mut self.started).await;
        tokio::time::timeout(WRITE_TIMEOUT, &mut self.result)
            .await
            .map_err(|_| timed_out())?
            .map_err(|_| WriteError::AgentUnavailable)?
            .accept()
    }

    /// Continue the completion owner in the background and return its worker
    /// handle for deterministic tests.
    pub(crate) fn detach(mut self) -> JoinHandle<()> {
        self.cancellation.disarm();
        self.worker
    }
}

/// Start a manual Disable Keys write. The owner keeps `pending` until native
/// I/O finishes, so cancellation cannot release the setting turn early.
pub(crate) fn write_manual_disable_keys(
    op: DeviceOp,
    pending: PendingWrite<Option<DisableKeysMask>>,
    desired: DisableKeysMask,
) -> Result<DisableKeysJob, WriteError> {
    let ticket = pending.ticket();
    spawn_disable_keys(op, ticket, desired, Some(pending), |_| {})
}

/// Re-apply a saved Disable Keys policy without giving it newer intent than a
/// manual request. The background owner retains all I/O guards until the
/// write-through path completes.
pub(crate) fn write_disabled_keys_in_background(
    op: DeviceOp,
    ticket: WriteTicket<Option<DisableKeysMask>>,
    desired: DisableKeysMask,
) -> Option<JoinHandle<()>> {
    let index = op.route.device_index();
    match spawn_disable_keys(op, ticket, desired, None, move |outcome| match outcome {
        Ok(state) => debug!(
            index,
            disabled = state.disabled.bits(),
            "disabled keys written"
        ),
        Err(WriteError::WriteSuperseded { .. }) => {
            debug!(index, "Disable Keys write superseded");
        }
        Err(error) => warn!(index, ?error, "Disable Keys write failed"),
    }) {
        Ok(job) => Some(job.detach()),
        Err(error) => {
            debug!(index, ?error, "Disable Keys background write skipped");
            None
        }
    }
}

fn spawn_disable_keys<Log>(
    op: DeviceOp,
    ticket: WriteTicket<Option<DisableKeysMask>>,
    desired: DisableKeysMask,
    pending: Option<PendingWrite<Option<DisableKeysMask>>>,
    log: Log,
) -> Result<DisableKeysJob, WriteError>
where
    Log: FnOnce(&Result<DisableKeysState, WriteError>) + Send + 'static,
{
    // Reject a missing route synchronously, but resolve again only after the
    // worker owns both ordering turns and the receiver lease.
    drop(op.resolve()?);

    // Published synchronously: a reread issued after reload must not overtake
    // this worker just because its OS thread has not started polling yet.
    let reservation = ticket.reserve();
    let cancelled = Arc::new(Cancellation::default());
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, result) = oneshot::channel();
    let (started_sender, started) = oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("openlogi-disable-keys".into())
        .spawn(move || {
            let outcome = match openlogi_core::worker::runtime() {
                Ok(runtime) => runtime.block_on(run_disable_keys(
                    op,
                    ticket,
                    desired,
                    reservation,
                    worker_cancelled,
                    started_sender,
                )),
                Err(error) => Err(WriteError::RuntimeInit {
                    message: error.to_string(),
                }),
            };
            log(&outcome);
            // Ownership travels with the result: dropping an unconsumed
            // message or failing to send it releases the pending intent only
            // after native I/O drained, without accepting an unsaved choice.
            let _ = sender.send(Completion { outcome, pending });
        })
        .map_err(|error| {
            WriteError::Hid(format!("could not start Disable Keys worker: {error}"))
        })?;

    Ok(DisableKeysJob {
        cancellation: RequesterCancellation::new(cancelled),
        started,
        result,
        worker,
    })
}

async fn run_disable_keys(
    op: DeviceOp,
    ticket: WriteTicket<Option<DisableKeysMask>>,
    desired: DisableKeysMask,
    reservation: Arc<ReservedTurn<Option<DisableKeysMask>>>,
    cancelled: Arc<Cancellation>,
    started: oneshot::Sender<()>,
) -> Result<DisableKeysState, WriteError> {
    let transport = op.access.transport_turns.turn_for(&op.route);
    let ready = async {
        reservation.wait().await;
        ticket
            .acquire(async {
                let lease = op.access.receiver_access.acquire_for_io().await;
                let turn = transport.lock().await;
                (lease, turn)
            })
            .await
    };
    let (_setting, (_lease, _transport)) = tokio::select! {
        biased;
        () = cancelled.wait() => return Err(timed_out()),
        guards = ready => guards,
    };
    let check = || {
        if cancelled.is_requested() {
            return Err(timed_out());
        }
        if !op.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        if !ticket.is_current() {
            return Err(WriteError::WriteSuperseded {
                operation: HidppOperation::WriteDisableKeys,
            });
        }
        Ok(())
    };
    check()?;
    let shared = op.resolve()?;
    let _ = started.send(());
    // Cancellation can stop admission, but must never drop an in-flight
    // native report. The write-through checkpoints still run between reports.
    openlogi_hid::set_disable_keys_write_through_on(&shared, desired, check).await
}

fn timed_out() -> WriteError {
    WriteError::RequestTimedOut {
        operation: HidppOperation::WriteDisableKeys,
    }
}

#[cfg(test)]
mod tests;

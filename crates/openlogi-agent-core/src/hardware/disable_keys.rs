//! Native ownership for complete Disable Keys writes.
//!
//! A requester can time out or cancel after the operating system has accepted
//! a report. The worker retains its ordering guards through that native write,
//! so a successor cannot enter the transport until the first write completes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use openlogi_hid::{DisableKeysMask, DisableKeysState, HidppOperation, WriteError};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use super::write_order::{PendingWrite, WriteTicket};
use super::{DeviceOp, WRITE_TIMEOUT};

/// A requester-owned cancellation signal for an owned native write.
struct RequesterCancellation {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl RequesterCancellation {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
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
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

/// A Disable Keys worker that owns the setting and transport turns through a
/// native HID write. Dropping the requester handle never aborts device I/O
/// already in progress.
#[must_use = "wait for Disable Keys or explicitly detach the completion owner"]
pub(crate) struct DisableKeysJob {
    cancellation: RequesterCancellation,
    started: oneshot::Receiver<()>,
    result: oneshot::Receiver<Result<DisableKeysState, WriteError>>,
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
    spawn_disable_keys(
        op,
        ticket,
        desired,
        move |outcome, cancelled| match outcome {
            Ok(state) if !cancelled && pending.confirm() => Ok(state),
            Ok(_) if cancelled => Err(timed_out()),
            Ok(_) => Err(WriteError::WriteSuperseded {
                operation: HidppOperation::WriteDisableKeys,
            }),
            Err(error) => Err(error),
        },
        |_| {},
    )
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
    match spawn_disable_keys(
        op,
        ticket,
        desired,
        |outcome, _| outcome,
        move |outcome| match outcome {
            Ok(state) => debug!(
                index,
                disabled = state.disabled.bits(),
                "disabled keys written"
            ),
            Err(WriteError::WriteSuperseded { .. }) => {
                debug!(index, "Disable Keys write superseded")
            }
            Err(error) => warn!(index, ?error, "Disable Keys write failed"),
        },
    ) {
        Ok(job) => Some(job.detach()),
        Err(error) => {
            debug!(index, ?error, "Disable Keys background write skipped");
            None
        }
    }
}

fn spawn_disable_keys<Complete, Log>(
    op: DeviceOp,
    ticket: WriteTicket<Option<DisableKeysMask>>,
    desired: DisableKeysMask,
    complete: Complete,
    log: Log,
) -> Result<DisableKeysJob, WriteError>
where
    Complete: FnOnce(Result<DisableKeysState, WriteError>, bool) -> Result<DisableKeysState, WriteError>
        + Send
        + 'static,
    Log: FnOnce(&Result<DisableKeysState, WriteError>) + Send + 'static,
{
    // Reject a missing route synchronously, but resolve again only after the
    // worker owns both ordering turns and the receiver lease.
    drop(op.resolve()?);

    let transport = op.access.transport_turns.turn_for(&op.route);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, result) = oneshot::channel();
    let (started_sender, started) = oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("openlogi-disable-keys".into())
        .spawn(move || {
            let outcome = match openlogi_core::worker::runtime() {
                Ok(runtime) => {
                    let write_cancelled = Arc::clone(&worker_cancelled);
                    let outcome = runtime.block_on(async move {
                        ticket
                            .write_or(
                                transport.lock(),
                                || async move {
                                    if write_cancelled.load(Ordering::Acquire) {
                                        return Err(timed_out());
                                    }
                                    let _lease = op.access.receiver_access.acquire_for_io().await;
                                    if !op.access.device_io.allows_io()
                                        || write_cancelled.load(Ordering::Acquire)
                                    {
                                        return Err(WriteError::DeviceNotFound);
                                    }
                                    let shared = op.resolve()?;
                                    let _ = started_sender.send(());
                                    openlogi_hid::set_disable_keys_on(&shared, desired).await
                                },
                                || async {
                                    Err(WriteError::WriteSuperseded {
                                        operation: HidppOperation::WriteDisableKeys,
                                    })
                                },
                            )
                            .await
                    });
                    complete(outcome, worker_cancelled.load(Ordering::Acquire))
                }
                Err(error) => Err(WriteError::RuntimeInit {
                    message: error.to_string(),
                }),
            };
            log(&outcome);
            // The requester can leave before native write completion.
            let _ = sender.send(outcome);
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

fn timed_out() -> WriteError {
    WriteError::RequestTimedOut {
        operation: HidppOperation::WriteDisableKeys,
    }
}

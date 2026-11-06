//! Native ownership of complete lighting transactions, independent of the
//! requester runtime. Cancellation signals the worker; it never aborts I/O.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use openlogi_core::color::Rgb;
use openlogi_device::write::LightingWrite;
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::{DeviceRoute, HidppOperation, LightingMethod, SharedChannel, WriteError};

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
type RouteLock = Arc<tokio::sync::Mutex<()>>;
static LIGHTING_LOCKS: LazyLock<Mutex<HashMap<String, RouteLock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
mod tests;

/// Cancellation read by the transaction owner between native requests.
pub struct LightingCancellation(Arc<AtomicBool>);

impl LightingCancellation {
    /// Whether the foreground caller stopped waiting for this transaction.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Where a lighting job's requester waits for its answer; taken by whichever
/// comes first, the worker's early answer or its final result.
type Answer = Arc<Mutex<Option<oneshot::Sender<Result<(), WriteError>>>>>;

/// Lets a long-running lighting worker answer its requester before it ends:
/// a drawn effect keeps painting after its first frame, but the requester
/// only waits for that frame.
pub struct LightingReady(Answer);

impl LightingReady {
    /// Report success to whoever waits on the job now. Later calls, and the
    /// job's own end, then report nothing more.
    pub fn ready(&self) {
        if let Some(sender) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = sender.send(Ok(()));
        }
    }
}

/// A lighting worker that owns its runtime and route lock until compensation
/// finishes. Dropping this handle requests cancellation, without blocking or
/// destroying the worker. Use [`Self::detach`] for background reapplication.
#[must_use = "wait for lighting or explicitly detach it"]
pub struct LightingJob {
    cancel: Option<Arc<AtomicBool>>,
    result: oneshot::Receiver<Result<(), WriteError>>,
}

impl LightingJob {
    /// Start an owned transaction. `operation` runs under the route's lighting
    /// lock; acquire receiver access and resolve the current publication inside
    /// it, not in the requester. The future must observe cancellation and drain
    /// native writes/rollback itself, as [`LightingWrite::apply_on`] does.
    pub fn spawn<F, Fut>(route: &DeviceRoute, operation: F) -> Result<Self, WriteError>
    where
        F: FnOnce(LightingCancellation) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), WriteError>>,
    {
        Self::spawn_long(route, |cancel, _ready| operation(cancel))
    }

    /// [`Self::spawn`] for a worker that runs on after answering: the job
    /// resolves at the first [`LightingReady::ready`], or at the end of
    /// `operation` if it never calls it. A requester that stops waiting
    /// still cancels the worker; once it has its answer, it no longer can.
    pub fn spawn_long<F, Fut>(route: &DeviceRoute, operation: F) -> Result<Self, WriteError>
    where
        F: FnOnce(LightingCancellation, LightingReady) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), WriteError>>,
    {
        let lock = LIGHTING_LOCKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.to_string())
            .or_default()
            .clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = LightingCancellation(Arc::clone(&cancel));
        let (sender, result) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let ready = LightingReady(Arc::clone(&sender));
        let route = route.clone();
        std::thread::Builder::new()
            .name("openlogi-rgb".into())
            .spawn(move || {
                let result = match openlogi_core::worker::runtime() {
                    Ok(runtime) => runtime.block_on(async {
                        let _guard = tokio::time::timeout(WAIT_TIMEOUT, lock.lock_owned())
                            .await
                            .map_err(|_| timed_out())?;
                        if cancellation.is_cancelled() {
                            return Err(timed_out());
                        }
                        // No timeout around this future: it owns native writes.
                        operation(cancellation, ready).await
                    }),
                    Err(error) => Err(WriteError::RuntimeInit {
                        message: error.to_string(),
                    }),
                };
                match &result {
                    Ok(()) => debug!(%route, "lighting transaction completed"),
                    Err(error) => warn!(%route, ?error, "lighting transaction failed"),
                }
                // A cancelled/detached requester may have dropped its receiver,
                // and a long worker may have answered already.
                if let Some(sender) = sender.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    let _ = sender.send(result);
                }
            })
            .map_err(|error| {
                WriteError::Hid(format!("could not start lighting worker: {error}"))
            })?;
        Ok(Self {
            cancel: Some(cancel),
            result,
        })
    }

    /// Wait up to five seconds. On elapse the worker is signalled but retains
    /// any in-flight native write and its recovery; a successor stays queued.
    /// For long-lived hosts such as the agent; standalone commands must use
    /// [`Self::finish`] so process exit cannot cut off recovery.
    pub async fn wait(mut self) -> Result<(), WriteError> {
        tokio::time::timeout(WAIT_TIMEOUT, &mut self.result)
            .await
            .map_err(|_| timed_out())?
            .map_err(|_| WriteError::AgentUnavailable)?
    }

    /// Wait through native write completion and cleanup before allowing a
    /// standalone command to exit. The transaction enforces its own deadline
    /// between writes, but a stuck native call cannot safely be cut short.
    pub async fn finish(mut self) -> Result<(), WriteError> {
        (&mut self.result)
            .await
            .map_err(|_| WriteError::AgentUnavailable)?
    }

    /// Continue in the background and log the result rather than cancelling
    /// when the requesting stack returns.
    pub fn detach(mut self) {
        self.cancel = None;
    }
}

impl Drop for LightingJob {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
    }
}

fn timed_out() -> WriteError {
    WriteError::RequestTimedOut {
        operation: HidppOperation::Lighting,
    }
}

/// Set a solid colour on an existing standalone channel with owned cleanup.
pub async fn set_keyboard_color_on(
    shared: &SharedChannel,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on(shared, LightingMethod::Auto, r, g, b).await
}

/// Set a solid colour via a chosen method with owned cleanup. Agent callers
/// use [`LightingJob::spawn`] so the worker also owns the receiver lease and
/// validates the inventory publication between requests.
pub async fn set_keyboard_color_with_on(
    shared: &SharedChannel,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let channel = shared.clone();
    let gate = crate::host::device_io_gate();
    LightingJob::spawn(shared.route(), move |cancel| async move {
        LightingWrite {
            method,
            color: Rgb::new(r, g, b),
            keys: Vec::new(),
            animation: None,
        }
        .apply_on(&channel, || cancel.is_cancelled(), || gate.allows_io())
        .await
        .map(drop)
    })?
    .finish()
    .await
}

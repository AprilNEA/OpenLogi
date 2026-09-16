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

const WAIT_BUDGET: Duration = Duration::from_secs(5);
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
        let lock = LIGHTING_LOCKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.to_string())
            .or_default()
            .clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = LightingCancellation(Arc::clone(&cancel));
        let (sender, result) = oneshot::channel();
        let route = route.clone();
        std::thread::Builder::new()
            .name("openlogi-rgb".into())
            .spawn(move || {
                let result = match openlogi_core::runtime::current_thread() {
                    Ok(runtime) => runtime.block_on(async {
                        let _guard = tokio::time::timeout(WAIT_BUDGET, lock.lock_owned())
                            .await
                            .map_err(|_| timed_out())?;
                        if cancellation.is_cancelled() {
                            return Err(timed_out());
                        }
                        // No timeout around this future: it owns native writes.
                        operation(cancellation).await
                    }),
                    Err(error) => Err(WriteError::RuntimeInit {
                        message: error.to_string(),
                    }),
                };
                match &result {
                    Ok(()) => debug!(%route, "lighting transaction completed"),
                    Err(error) => warn!(%route, ?error, "lighting transaction failed"),
                }
                // A cancelled/detached requester may have dropped its receiver.
                let _ = sender.send(result);
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
        tokio::time::timeout(WAIT_BUDGET, &mut self.result)
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
        }
        .apply_on(&channel, || cancel.is_cancelled(), || gate.allows_io())
        .await
    })?
    .finish()
    .await
}

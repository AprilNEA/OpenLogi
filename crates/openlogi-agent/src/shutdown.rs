//! Graceful shutdown: process signals and deliberate UI requests converge on
//! the exit path that restores device state and releases the input hook.

use std::sync::OnceLock;

use openlogi_hook::Hook;
use tokio::sync::mpsc;
use tracing::info;
use tracing::warn;

use crate::startup::InputServices;

static REQUEST: OnceLock<mpsc::UnboundedSender<&'static str>> = OnceLock::new();

/// Create the process-wide deliberate-shutdown channel. The lifecycle owns
/// the receiver; tray callbacks can request the same graceful exit path
/// without terminating from their native UI threads.
pub(crate) fn request_channel() -> mpsc::UnboundedReceiver<&'static str> {
    let (sender, receiver) = mpsc::unbounded_channel();
    assert!(
        REQUEST.set(sender).is_ok(),
        "shutdown request channel must be initialized exactly once"
    );
    receiver
}

/// Ask the lifecycle thread to perform a deliberate, graceful process exit.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn request(reason: &'static str) {
    if REQUEST
        .get()
        .is_none_or(|sender| sender.send(reason).is_err())
    {
        warn!(reason, "could not deliver graceful shutdown request");
    }
}

/// A future that fires when `signal` does, or never when the handler could not
/// be installed.
#[cfg(unix)]
async fn fires(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// The shutdown sources, installed once and consumed by whichever lifecycle
/// stage is currently in charge.
pub(crate) struct ShutdownSignals {
    requests: mpsc::UnboundedReceiver<&'static str>,
    #[cfg(unix)]
    sigterm: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    sigint: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    /// Install the shutdown-signal handlers. A handler that cannot be
    /// installed is `None`, which simply never fires.
    #[cfg(unix)]
    pub(crate) fn install(requests: mpsc::UnboundedReceiver<&'static str>) -> Self {
        fn listen(kind: tokio::signal::unix::SignalKind) -> Option<tokio::signal::unix::Signal> {
            tokio::signal::unix::signal(kind)
                .inspect_err(|error| warn!(%error, ?kind, "could not install signal handler"))
                .ok()
        }
        Self {
            requests,
            sigterm: listen(tokio::signal::unix::SignalKind::terminate()),
            sigint: listen(tokio::signal::unix::SignalKind::interrupt()),
        }
    }

    /// No signals exist off unix.
    #[cfg(not(unix))]
    pub(crate) fn install(requests: mpsc::UnboundedReceiver<&'static str>) -> Self {
        Self { requests }
    }

    /// Resolves on the first signal that means *stop now*: `SIGTERM` from
    /// launchd or a takeover, `SIGINT` from a dev-run Ctrl-C — both would
    /// otherwise kill the process with the event tap still armed.
    #[cfg(unix)]
    pub(crate) async fn recv(&mut self) -> &'static str {
        tokio::select! {
            Some(reason) = self.requests.recv() => reason,
            () = fires(&mut self.sigterm) => "SIGTERM",
            () = fires(&mut self.sigint) => "SIGINT",
        }
    }

    /// No signal to wait for off unix; the future simply never resolves.
    #[cfg(not(unix))]
    pub(crate) async fn recv(&mut self) -> &'static str {
        match self.requests.recv().await {
            Some(reason) => reason,
            None => std::future::pending().await,
        }
    }
}

/// Release the input hook, then end the process. The run loop is not the
/// process — macOS keeps the AppKit tray loop on the main thread — so the
/// exit has to be explicit, and it must run the hook's destructor.
pub(crate) fn release_hook_and_exit(
    hook: Option<Hook>,
    inputs: &mut InputServices,
    reason: &str,
) -> ! {
    info!(reason, "releasing the input hook and exiting");
    drop(hook);
    inputs.shutdown();
    #[expect(
        clippy::exit,
        reason = "a signalled shutdown must end the process, and the loop that observed it runs off the main thread"
    )]
    std::process::exit(0)
}

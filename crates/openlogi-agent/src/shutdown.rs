//! Signal-driven shutdown: the `SIGTERM`/`SIGINT` listeners and the exit
//! path that releases the input hook before the process ends.

use openlogi_hook::Hook;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use crate::startup::InputServices;

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

/// The process's stop-signal listeners, installed once right after bootstrap
/// and consumed by whichever lifecycle stage is currently in charge — the
/// dormancy gate first, the run loop after arming. Owning them as one value
/// is what hands them from stage to stage without a loose pair of receivers.
pub(crate) struct ShutdownSignals {
    #[cfg(unix)]
    sigterm: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    sigint: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    /// Install the shutdown-signal handlers. A handler that cannot be
    /// installed is `None`, which simply never fires.
    #[cfg(unix)]
    pub(crate) fn install() -> Self {
        fn listen(kind: tokio::signal::unix::SignalKind) -> Option<tokio::signal::unix::Signal> {
            tokio::signal::unix::signal(kind)
                .inspect_err(|error| warn!(%error, ?kind, "could not install signal handler"))
                .ok()
        }
        Self {
            sigterm: listen(tokio::signal::unix::SignalKind::terminate()),
            sigint: listen(tokio::signal::unix::SignalKind::interrupt()),
        }
    }

    /// No signals exist off unix.
    #[cfg(not(unix))]
    pub(crate) fn install() -> Self {
        Self {}
    }

    /// Resolves on the first signal that means *stop now*: `SIGTERM` from
    /// launchd (logout, `bootout`) or from an incoming agent's takeover,
    /// `SIGINT` from a dev-run Ctrl-C. Both default to killing the process
    /// where it stands, which on macOS would strand an armed HID event tap in
    /// the system's tap chain.
    #[cfg(unix)]
    pub(crate) async fn recv(&mut self) {
        tokio::select! {
            () = fires(&mut self.sigterm) => {}
            () = fires(&mut self.sigint) => {}
        }
    }

    /// No signal to wait for off unix; the future simply never resolves.
    #[cfg(not(unix))]
    pub(crate) async fn recv(&mut self) {
        std::future::pending::<()>().await;
    }
}

/// Release the input hook, then end the process.
///
/// Dropping the hook detaches the macOS event tap; a signal's default
/// disposition would have killed the process with the tap still armed, and so
/// would any other way of leaving that skips destructors. The agent's run loop
/// is not the process — macOS keeps the AppKit tray loop on the main thread —
/// so the exit has to be explicit.
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

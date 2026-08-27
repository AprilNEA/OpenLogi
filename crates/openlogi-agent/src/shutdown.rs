//! Signal-driven shutdown: the `SIGTERM`/`SIGINT` listeners and the exit
//! path that releases the input hook before the process ends.

use openlogi_hook::Hook;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use crate::InputServices;

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

/// Resolves on the first signal that means *stop now*: `SIGTERM` from launchd
/// (logout, `bootout`) or from an incoming agent's takeover, `SIGINT` from a
/// dev-run Ctrl-C. Both default to killing the process where it stands, which
/// on macOS would strand an armed HID event tap in the system's tap chain.
#[cfg(unix)]
pub(crate) async fn shutdown_signal(
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    sigint: &mut Option<tokio::signal::unix::Signal>,
) {
    tokio::select! {
        () = fires(sigterm) => {}
        () = fires(sigint) => {}
    }
}

/// No signal to wait for off unix; the arm simply never fires.
#[cfg(not(unix))]
pub(crate) async fn shutdown_signal(_sigterm: &mut Option<()>, _sigint: &mut Option<()>) {
    std::future::pending::<()>().await;
}

/// Install the shutdown-signal handlers, `(SIGTERM, SIGINT)`. A handler that
/// cannot be installed is `None`, which simply never fires.
#[cfg(unix)]
pub(crate) fn shutdown_signals() -> (
    Option<tokio::signal::unix::Signal>,
    Option<tokio::signal::unix::Signal>,
) {
    fn listen(kind: tokio::signal::unix::SignalKind) -> Option<tokio::signal::unix::Signal> {
        tokio::signal::unix::signal(kind)
            .inspect_err(|error| warn!(%error, ?kind, "could not install signal handler"))
            .ok()
    }
    (
        listen(tokio::signal::unix::SignalKind::terminate()),
        listen(tokio::signal::unix::SignalKind::interrupt()),
    )
}

#[cfg(not(unix))]
pub(crate) fn shutdown_signals() -> (Option<()>, Option<()>) {
    (None, None)
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

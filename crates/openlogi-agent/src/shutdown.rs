//! Process-shutdown requests, `SIGTERM`/`SIGINT` listeners, and the one exit
//! path that releases firmware capture and the input hook before process end.

use std::path::PathBuf;

use openlogi_hook::Hook;
use tokio::sync::{mpsc, oneshot};
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use crate::startup::InputServices;

/// A non-signal request for the process lifecycle owner.
pub(crate) enum ShutdownRequest {
    /// The user chose Quit from the macOS or Windows tray.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    TrayQuit {
        /// Kept alive through graceful teardown. The tray only falls back to
        /// direct exit if this sender is dropped because the core returned or
        /// panicked instead of ending the process.
        core_guard: oneshot::Sender<()>,
    },
    /// The installed application disappeared from disk.
    Uninstalled,
    /// The executable was replaced and the process should become `path`.
    Restart {
        path: PathBuf,
        /// Sent only when scheduling or `exec` failed and the binary watcher
        /// should resume observing for another settled replacement.
        retry: oneshot::Sender<()>,
    },
}

/// Sender cloned into the tray and executable watcher. Those threads request
/// transitions; the lifecycle remains the sole authority that performs them.
pub(crate) type ShutdownRequestSender = mpsc::UnboundedSender<ShutdownRequest>;

/// The single non-signal shutdown source carried through every lifecycle
/// stage so process replacement cannot bypass armed firmware ownership.
pub(crate) struct ShutdownRequests(mpsc::UnboundedReceiver<ShutdownRequest>);

impl ShutdownRequests {
    pub(crate) async fn recv(&mut self) -> Option<ShutdownRequest> {
        self.0.recv().await
    }
}

/// Build the process-wide non-signal shutdown channel.
pub(crate) fn request_channel() -> (ShutdownRequestSender, ShutdownRequests) {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, ShutdownRequests(rx))
}

/// Ask the async lifecycle to quit, then block this tray thread until the
/// process ends. There is deliberately no elapsed-time fallback: a timer
/// cannot distinguish a dead core from legitimate firmware restoration and
/// can preempt the exact cleanup this handoff exists to protect.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn request_tray_quit(
    requests: Option<&ShutdownRequestSender>,
    fallback_status: i32,
) -> ! {
    let (core_guard, core_ended) = oneshot::channel();
    let sent = requests.is_some_and(|requests| {
        requests
            .send(ShutdownRequest::TrayQuit { core_guard })
            .is_ok()
    });
    if sent {
        // Successful graceful shutdown calls `process::exit`, so this returns
        // only when unwinding or an early core return drops the guard.
        let _ = core_ended.blocking_recv();
        tracing::warn!("agent core ended without exiting after tray Quit — exiting directly");
    } else {
        tracing::warn!("no live agent core to accept tray Quit — exiting directly");
    }
    #[expect(
        clippy::exit,
        reason = "fallback only: a missing or unwound core cannot return a status through an AppKit or win32 tray callback"
    )]
    std::process::exit(fallback_status)
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

/// The stop-signal listeners, installed once and consumed by whichever
/// lifecycle stage is currently in charge.
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
    /// launchd or a takeover, `SIGINT` from a dev-run Ctrl-C — both would
    /// otherwise kill the process with the event tap still armed.
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

/// Release the input hook, then end the process. The run loop is not the
/// process — macOS keeps the AppKit tray loop on the main thread — so the
/// exit has to be explicit, and it must run the hook's destructor.
pub(crate) fn release_hook_and_exit(
    hook: Option<Hook>,
    inputs: &mut InputServices,
    reason: &str,
    _tray_guard: Option<oneshot::Sender<()>>,
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

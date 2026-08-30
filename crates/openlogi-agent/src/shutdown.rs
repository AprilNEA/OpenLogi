//! Signal-driven shutdown and the process-exit funnel.
//!
//! `SIGTERM`/`SIGINT` release the input hook then join this crate's one
//! exit path. Tray Quit, AppKit-loop return, and binary-watch relaunch
//! cannot see [`InputServices`], so they join the same funnel: flush any
//! open pan/zoom session (idempotent) and only then `process::exit`.
//! `std::process::exit` lives in exactly one function so a later exit
//! site is correct by construction.

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

/// Work that must finish before this process image is gone.
///
/// `process::exit` and `exec` skip `Drop`. Hold-mode pan/zoom is a real OS
/// gesture (or a held Ctrl on Linux/Windows); leaving it open strands the
/// focused app. This seals rather than flushes: the gesture watcher is not
/// joined here, so a thread still streaming would otherwise reopen a pinch
/// between the last flush and the exit. Sealing is idempotent and a no-op
/// when nothing is open. Isolated from [`flush_and_exit`] so the Linux `exec`
/// relaunch can run the same work without ending a process that is about to
/// be replaced, and so tests can prove it without terminating the harness.
///
/// A panic here must not prevent the caller from exiting: an exit path that
/// hangs or unwinds is worse than a stuck pinch.
pub(crate) fn prepare_process_exit() {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        openlogi_inject::seal_gesture_sessions();
    }));
}

/// The crate's only `process::exit`. Every death path that cannot return an
/// [`std::process::ExitCode`] to `main` comes through here.
pub(crate) fn flush_and_exit(code: i32) -> ! {
    prepare_process_exit();
    #[expect(
        clippy::exit,
        reason = "AppKit, the Windows tray pump, and the binary-watch thread cannot return an ExitCode to main; this is the crate's only process::exit, after the gesture flush"
    )]
    std::process::exit(code)
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
    flush_and_exit(0)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::prepare_process_exit;

    /// Strip `//` comments so doc-comments mentioning `process::exit` do not
    /// look like call sites. `https://` is truncated too; that cannot hide a
    /// real `process::exit`.
    fn code_without_line_comments(src: &str) -> String {
        src.lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn production_src(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    fn collect_rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("agent src should be readable") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "bin") {
                    continue;
                }
                collect_rust_sources(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    fn rust_sources_under(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_rust_sources(root, &mut files);
        files.sort();
        files
    }

    #[test]
    fn prepare_process_exit_is_safe_when_nothing_is_open() {
        prepare_process_exit();
        prepare_process_exit();
    }

    #[test]
    fn every_process_exit_goes_through_the_funnel() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut elsewhere = Vec::new();
        let mut funnel_exits = 0;
        for path in rust_sources_under(&root) {
            let src = std::fs::read_to_string(&path).expect("source should be readable");
            let code = code_without_line_comments(production_src(&src));
            let hits = code.matches("std::process::exit").count();
            if hits == 0 {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "shutdown.rs") {
                funnel_exits += hits;
            } else {
                elsewhere.push(path.display().to_string());
            }
        }
        assert!(
            elsewhere.is_empty(),
            "process::exit must live only in flush_and_exit; found in {elsewhere:?}"
        );
        assert_eq!(
            funnel_exits, 1,
            "shutdown.rs must contain exactly one process::exit (the funnel)"
        );
    }
}

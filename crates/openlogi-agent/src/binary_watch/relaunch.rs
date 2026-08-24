//! How a restart is actually carried out, per OS.
//!
//! [`super`] decides *that* the agent should become the binary now on disk;
//! this module knows what that costs on each platform, and the three answers
//! have nothing in common:
//!
//! - **Linux and the other unixes** `exec` the new image in place, which keeps
//!   the pid, the singleton lock, and the IPC socket.
//! - **macOS** cannot: after `exec` the new program continues on the calling
//!   thread, while AppKit insists the status item be created from the process
//!   main thread. So it schedules a detached successor and exits, and the
//!   successor waits for this process to release the singleton lock. Going
//!   through LaunchServices (`open`) rather than spawning the binary directly
//!   is what preserves the helper's TCC identity — see
//!   `.claude/rules/objc-ffi.md` and the permissions skill.
//! - **Windows** has no `exec` at all: it exits and lets the GUI's socket-down
//!   spawn (or the next login) start the replacement.
//!
//! The macOS path also serves the Input Monitoring grant, which needs the same
//! "leave and come back" move for an unrelated reason.

use std::path::Path;

use tracing::info;
// Only the unix relaunches can fail in a way worth reporting; Windows just
// exits.
#[cfg(unix)]
use tracing::warn;

/// Restart this process as the new binary at `path`.
///
/// The singleton file lock and the IPC socket close with the old image and are
/// re-acquired by the new one; the listener unlinks the stale socket file on
/// bind. If scheduling the restart fails the process is still intact, so return
/// and let the watch loop retry once the file settles again.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn restart(path: &Path) {
    use std::os::unix::process::CommandExt as _;
    info!(
        path = %path.display(),
        "executable changed on disk — restarting as the new binary"
    );
    // Forward our argv (none today) so a future flag survives the restart.
    let err = std::process::Command::new(path)
        .args(std::env::args_os().skip(1))
        .exec();
    warn!(error = %err, "exec of the updated agent failed — keeping the current image and retrying");
}

/// macOS cannot safely `exec` the replacement from this watcher thread: after
/// `exec`, the new program continues on the calling thread, while AppKit expects
/// the status item to be created from the process main thread. Relaunch the
/// packaged helper through LaunchServices (preserving its TCC identity), or a
/// bare dev binary directly, after this process has had time to exit and release
/// the singleton lock.
#[cfg(target_os = "macos")]
pub(super) fn restart(path: &Path) {
    info!(
        path = %path.display(),
        "executable changed on disk — relaunching as the new macOS agent"
    );
    if let Err(e) = schedule_macos_relaunch_and_exit(path) {
        warn!(error = %e, "could not schedule updated agent relaunch — keeping the current image and retrying");
    }
}

/// Relaunch the macOS agent after Input Monitoring changes.
///
/// macOS does not apply a new Input Monitoring decision to the running process.
/// The successor starts only after this process exits and releases its
/// singleton lock and IPC socket. If the relaunch cannot be scheduled, the
/// current process stays alive so the user can restart it manually.
#[cfg(target_os = "macos")]
pub fn restart_after_input_monitoring_change() {
    let path = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            warn!(error = %e, "could not resolve own executable after Input Monitoring was granted — restart the agent manually");
            return;
        }
    };
    info!("Input Monitoring changed — relaunching the macOS agent");
    if let Err(e) = schedule_macos_relaunch_and_exit(&path) {
        warn!(error = %e, "could not schedule agent relaunch after Input Monitoring was granted — restart the agent manually");
    }
}

#[cfg(target_os = "macos")]
fn schedule_macos_relaunch(path: &Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new("/bin/sh");
    if let Some(bundle) = helper_bundle(path) {
        command
            .arg("-c")
            .arg("sleep 0.5; exec /usr/bin/open -g -n \"$1\"")
            .arg("openlogi-relaunch")
            .arg(bundle);
    } else {
        command
            .arg("-c")
            .arg("path=$1; shift; sleep 0.5; exec \"$path\" \"$@\"")
            .arg("openlogi-relaunch")
            .arg(path)
            .args(std::env::args_os().skip(1));
    }
    command.spawn().map(|_| ())
}

#[cfg(target_os = "macos")]
fn schedule_macos_relaunch_and_exit(path: &Path) -> std::io::Result<()> {
    schedule_macos_relaunch(path)?;
    #[expect(
        clippy::exit,
        reason = "the delayed successor is already scheduled and waits for this process to release the singleton lock and IPC socket"
    )]
    std::process::exit(0)
}

/// The `.app` root of a packaged helper binary, `None` for a bare dev binary.
#[cfg(target_os = "macos")]
fn helper_bundle(path: &Path) -> Option<&Path> {
    let bundle = path.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
}

/// Windows has no `exec`: exit cleanly and let the GUI's socket-down spawn
/// retry (or the next login's autostart) start the replaced binary. A
/// spawn-before-exit handover would lose the race against the singleton lock
/// this process still holds.
#[cfg(windows)]
pub(super) fn restart(path: &Path) {
    info!(
        path = %path.display(),
        "executable changed on disk — exiting so the new binary can start"
    );
    #[expect(
        clippy::exit,
        reason = "windows has no `exec`, and this watcher thread cannot return a status to `main`, which is blocked on the agent core; releasing the singleton lock by exiting is what lets the replaced binary start"
    )]
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
#[cfg(test)]
mod tests {
    #[test]
    fn macos_helper_bundle_is_detected_from_packaged_binary_path() {
        use super::helper_bundle;
        use std::path::Path;

        let binary = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        let bundle =
            Path::new("/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app");
        assert_eq!(helper_bundle(binary), Some(bundle));
        assert_eq!(helper_bundle(Path::new("/tmp/openlogi-agent")), None);
    }
}

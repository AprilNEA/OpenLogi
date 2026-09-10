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
//!   main thread. So it schedules a detached successor that waits for this
//!   process to exit. Going through LaunchServices (`open`) rather than
//!   spawning the binary directly preserves the helper's TCC identity — see
//!   `.claude/rules/objc-ffi.md` and the permissions skill.
//! - **Windows** has no `exec`; the lifecycle exits after teardown and lets
//!   the GUI's socket-down spawn (or the next login) start the replacement.
//!
//! The macOS path also serves the Input Monitoring grant, which needs the same
//! "leave and come back" move for an unrelated reason.

#[cfg(unix)]
use std::path::Path;

#[cfg(all(unix, not(target_os = "macos")))]
use tracing::info;

/// Restart this process as the new binary at `path`.
///
/// The lifecycle has already confirmed every HID++ manager stopped before
/// calling this. If `exec` fails, return the error so it can safely start a new
/// manager fleet before asking the binary watcher to retry.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn replace_process(path: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    info!(
        path = %path.display(),
        "executable changed on disk — restarting as the new binary"
    );
    // Forward our argv (none today) so a future flag survives the restart.
    std::process::Command::new(path)
        .args(std::env::args_os().skip(1))
        .exec()
}

/// Schedule a macOS successor without ending the current process.
///
/// The packaged helper goes through LaunchServices to preserve its TCC
/// identity; a bare dev binary starts directly. The successor waits for this
/// exact PID rather than a fixed delay, so graceful firmware teardown cannot
/// make it lose the singleton-lock race and disappear.
#[cfg(target_os = "macos")]
pub(crate) fn schedule(path: &Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new("/bin/sh");
    let pid = std::process::id().to_string();
    if let Some(bundle) = helper_bundle(path) {
        command
            .arg("-c")
            .arg(
                "pid=$1; bundle=$2; while /bin/kill -0 \"$pid\" 2>/dev/null; do /bin/sleep 0.1; done; exec /usr/bin/open -g -n \"$bundle\"",
            )
            .arg("openlogi-relaunch")
            .arg(&pid)
            .arg(bundle);
    } else {
        command
            .arg("-c")
            .arg(
                "pid=$1; path=$2; shift 2; while /bin/kill -0 \"$pid\" 2>/dev/null; do /bin/sleep 0.1; done; exec \"$path\" \"$@\"",
            )
            .arg("openlogi-relaunch")
            .arg(&pid)
            .arg(path)
            .args(std::env::args_os().skip(1));
    }
    command.spawn().map(|_| ())
}

/// Schedule the macOS agent's required relaunch after Input Monitoring is
/// granted. Returns whether scheduling succeeded; the lifecycle performs the
/// actual graceful exit.
///
/// macOS does not apply a new Input Monitoring grant to the running process.
#[cfg(target_os = "macos")]
pub(crate) fn schedule_after_input_monitoring_grant() -> bool {
    use tracing::{info, warn};

    let path = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            warn!(error = %e, "could not resolve own executable after Input Monitoring was granted — restart the agent manually");
            return false;
        }
    };
    info!("Input Monitoring granted — relaunching the macOS agent");
    match schedule(&path) {
        Ok(()) => true,
        Err(e) => {
            warn!(error = %e, "could not schedule agent relaunch after Input Monitoring was granted — restart the agent manually");
            false
        }
    }
}

/// The `.app` root of a packaged helper binary, `None` for a bare dev binary.
#[cfg(target_os = "macos")]
fn helper_bundle(path: &Path) -> Option<&Path> {
    let bundle = path.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
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

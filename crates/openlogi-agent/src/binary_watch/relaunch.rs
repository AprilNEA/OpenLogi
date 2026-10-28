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
    successor(path, std::process::id()).spawn().map(|_| ())
}

/// The shell that waits for `pid` to exit and then starts the agent at `path`.
///
/// The shell leads its own process group. Under launchd the agent leads the
/// job's process group, and when the job's process exits launchd kills every
/// process still in that group (launchd.plist(5), `AbandonProcessGroup`). A
/// successor left in the agent's group dies with the agent it waits for, and
/// `KeepAlive = {SuccessfulExit: false}` does not respawn the clean exit, so
/// no agent runs until the GUI starts one (#1798).
#[cfg(target_os = "macos")]
fn successor(path: &Path, pid: u32) -> std::process::Command {
    use std::os::unix::process::CommandExt as _;

    let mut command = std::process::Command::new("/bin/sh");
    command.process_group(0);
    let pid = pid.to_string();
    if let Some(bundle) = openlogi_core::brand::helper_bundle_root(path) {
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
    command
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::path::Path;
    use std::process::Command;

    /// The process group that `ps` reports for `pid`.
    fn process_group(pid: u32) -> u32 {
        let output = Command::new("/bin/ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        String::from_utf8(output.stdout)
            .expect("ps prints ASCII")
            .trim()
            .parse()
            .expect("ps prints one process group id")
    }

    /// launchd kills the exiting job's whole process group, so the successor
    /// must not share the agent's group (#1798).
    #[test]
    fn the_successor_leaves_the_agents_process_group() {
        // The shell waits for this live test process, so it stays in its wait
        // loop and never starts the target.
        let mut shell = super::successor(Path::new("/usr/bin/true"), std::process::id())
            .spawn()
            .expect("spawn the successor shell");
        let shell_group = process_group(shell.id());
        let own_group = process_group(std::process::id());
        shell.kill().expect("stop the successor shell");
        shell.wait().expect("reap the successor shell");
        assert_eq!(
            shell_group,
            shell.id(),
            "the successor leads its own process group"
        );
        assert_ne!(
            shell_group, own_group,
            "the successor shares the process group of the agent it waits for"
        );
    }
}

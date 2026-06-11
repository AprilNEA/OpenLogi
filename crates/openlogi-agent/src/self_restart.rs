//! Restart the agent when its on-disk executable is replaced.
//!
//! An app update (Homebrew cask, the in-app updater, a dev rebuild) swaps the
//! bundle on disk while the old agent keeps running. launchd only restarts the
//! process when it *exits*, so nothing would pick up the new binary until the
//! next login — and a GUI launched from the new bundle refuses the old agent's
//! IPC protocol on a version bump, sitting on its connecting screen with no way
//! forward. Watching our own executable and replacing the process image the
//! moment it changes keeps "the running agent is the installed binary" true
//! within one tick, with no launchd or GUI involvement — remapping continues
//! even in setups where nothing would respawn a plain exit (autostart off,
//! GUI closed).

use std::path::Path;
use std::time::{Duration, SystemTime};

use tracing::{info, warn};

/// How often to stat the executable: one `metadata` call per tick — noise next
/// to the 2 s HID enumerate — while keeping the update-to-restart window short.
const PERIOD: Duration = Duration::from_secs(10);

/// What "the binary changed" means: a different size or mtime at the same
/// path. Every real update path rewrites the file, so content hashing would
/// buy nothing.
type Fingerprint = (u64, SystemTime);

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Spawn the watcher thread. The executable path and its baseline fingerprint
/// are resolved once, up front; if either fails the watch is disabled (logged)
/// rather than guessing at a path.
pub fn spawn() {
    let Ok(path) = std::env::current_exe() else {
        warn!("could not resolve own executable — binary-update watch disabled");
        return;
    };
    let Some(baseline) = fingerprint(&path) else {
        warn!(
            path = %path.display(),
            "could not stat own executable — binary-update watch disabled"
        );
        return;
    };
    let spawn_result = std::thread::Builder::new()
        .name("openlogi-binary-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(PERIOD);
                // A vanished file is *not* a change: mid-replace the old inode
                // is unlinked before the new file lands, so wait for a readable
                // replacement before acting.
                if fingerprint(&path).is_some_and(|now| now != baseline) {
                    restart(&path);
                }
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn the binary-update watch thread");
    }
}

/// Replace this process with the new binary at `path`.
///
/// `exec` keeps the pid, so launchd's bookkeeping — including the
/// `SuccessfulExit: false` semantics that make the tray's Quit final — is
/// untouched, and no external respawner is needed. The singleton file lock and
/// the IPC socket close with the old image (Rust opens fds `CLOEXEC`) and are
/// re-acquired by the new one; the listener unlinks the stale socket file on
/// bind. If `exec` itself fails, exit non-zero so launchd's crash-respawn
/// (where installed) starts the new binary instead.
#[cfg(unix)]
fn restart(path: &Path) -> ! {
    use std::os::unix::process::CommandExt as _;
    info!(
        path = %path.display(),
        "executable changed on disk — restarting as the new binary"
    );
    let err = std::process::Command::new(path).exec();
    warn!(error = %err, "exec of the updated agent failed — exiting for the respawner");
    std::process::exit(1);
}

/// Windows has no `exec`: exit cleanly and let the GUI's socket-down spawn
/// retry (or the next login's autostart) start the replaced binary. A
/// spawn-before-exit handover would lose the race against the singleton lock
/// this process still holds.
#[cfg(windows)]
fn restart(path: &Path) -> ! {
    info!(
        path = %path.display(),
        "executable changed on disk — exiting so the new binary can start"
    );
    std::process::exit(0);
}

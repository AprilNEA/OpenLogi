//! Restart the agent when its on-disk executable is replaced.
//!
//! An app update (Homebrew cask, the in-app updater, a dev rebuild) swaps the
//! bundle on disk while the old agent keeps running. launchd only restarts the
//! process when it *exits*, so nothing would pick up the new binary until the
//! next login — and a GUI launched from the new bundle refuses the old agent's
//! IPC protocol on a version bump, sitting on its connecting screen with no way
//! forward. Watching our own executable and replacing the process image once it
//! changes keeps "the running agent is the installed binary" true within a few
//! ticks, with no launchd or GUI involvement. On macOS the replacement is a
//! short delayed relaunch rather than an in-thread `exec`: the agent owns an
//! AppKit status item, and the new AppKit process must start from the normal
//! process main thread or the menu-bar item can render as an empty slot.
//!
//! Limitation: the path is resolved once via `current_exe`, which returns the
//! fully-resolved target (`/proc/self/exe` on Linux). Installs that update by
//! flipping a symlink to a new immutable payload (Nix profiles) never change
//! the resolved file, so this watcher can't see those updates; every shipped
//! channel replaces the binary in place.

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

/// One watch tick: what the new fingerprint means, given what the last tick saw.
///
/// A change must hold still for two consecutive ticks before it triggers a
/// restart: a non-atomic replacement (`cp`, the linker rewriting the file in
/// place) is observable mid-write, and exec'ing a half-written image would kill
/// the agent instead of updating it. `pending` carries the candidate between
/// ticks.
fn assess(
    baseline: Fingerprint,
    pending: Option<Fingerprint>,
    now: Option<Fingerprint>,
) -> (Option<Fingerprint>, bool) {
    match now {
        // A vanished file is *not* a change: mid-replace the old inode is
        // unlinked before the new file lands, so wait for a readable
        // replacement before even arming.
        None => (None, false),
        Some(now) if now == baseline => (None, false),
        // Same non-baseline fingerprint twice in a row — the write has settled.
        Some(now) if pending == Some(now) => (Some(now), true),
        // First sighting (or still churning): arm and re-check next tick.
        Some(now) => (Some(now), false),
    }
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
            let mut pending: Option<Fingerprint> = None;
            loop {
                std::thread::sleep(PERIOD);
                let restart_now;
                (pending, restart_now) = assess(baseline, pending, fingerprint(&path));
                if restart_now {
                    restart(&path);
                    // Only reached when the exec failed (a broken or still-
                    // churning file). Disarm so the retry needs a fresh
                    // two-tick settle — staying alive on the old image beats
                    // dying in setups with no respawner.
                    pending = None;
                }
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn the binary-update watch thread");
    }
}

/// Restart this process as the new binary at `path`.
///
/// The singleton file lock and the IPC socket close with the old image and are
/// re-acquired by the new one; the listener unlinks the stale socket file on
/// bind. If scheduling the restart fails the process is still intact, so return
/// and let the watch loop retry once the file settles again.
#[cfg(all(unix, not(target_os = "macos")))]
fn restart(path: &Path) {
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
fn restart(path: &Path) {
    info!(
        path = %path.display(),
        "executable changed on disk — relaunching as the new macOS agent"
    );
    match schedule_macos_relaunch(path) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            warn!(error = %e, "could not schedule updated agent relaunch — keeping the current image and retrying");
        }
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
fn restart(path: &Path) {
    info!(
        path = %path.display(),
        "executable changed on disk — exiting so the new binary can start"
    );
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::{Fingerprint, assess};
    use std::time::{Duration, SystemTime};

    fn fp(len: u64, secs: u64) -> Fingerprint {
        (len, SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn restarts_only_after_a_change_settles() {
        let baseline = fp(100, 1);
        let new = fp(200, 2);
        // First differing sighting arms but does not restart…
        assert_eq!(assess(baseline, None, Some(new)), (Some(new), false));
        // …the same fingerprint on the next tick restarts.
        assert_eq!(assess(baseline, Some(new), Some(new)), (Some(new), true));
    }

    #[test]
    fn churning_writes_keep_rearming() {
        let baseline = fp(100, 1);
        let half = fp(150, 2);
        let full = fp(200, 3);
        // A still-growing file never matches its previous sighting.
        assert_eq!(
            assess(baseline, Some(half), Some(full)),
            (Some(full), false)
        );
    }

    #[test]
    fn vanished_and_reverted_files_disarm() {
        let baseline = fp(100, 1);
        let new = fp(200, 2);
        // Mid-replace ENOENT: not a change, and any armed candidate is dropped.
        assert_eq!(assess(baseline, Some(new), None), (None, false));
        // Back at the baseline (e.g. a rollback): disarm too.
        assert_eq!(assess(baseline, Some(new), Some(baseline)), (None, false));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_helper_bundle_is_detected_from_packaged_binary_path() {
        use super::helper_bundle;
        use std::path::Path;

        let binary = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
        );
        let bundle =
            Path::new("/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app");
        assert_eq!(helper_bundle(binary), Some(bundle));
        assert_eq!(helper_bundle(Path::new("/tmp/openlogi-agent")), None);
    }
}

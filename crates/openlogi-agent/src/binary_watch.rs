//! Restart the agent when its on-disk executable is replaced — and stop it when
//! that executable goes away for good.
//!
//! An app update (Homebrew cask, the in-app updater, a dev rebuild) swaps the
//! bundle on disk while the old agent keeps running. launchd only restarts the
//! process when it *exits*, so nothing would pick up the new binary until the
//! next login — and a GUI launched from the new bundle refuses the old agent's
//! IPC protocol on a version bump, sitting on its connecting screen with no way
//! forward. Watching our own executable and replacing the process image once it
//! changes keeps "the running agent is the installed binary" true within a few
//! ticks, with no launchd or GUI involvement. How a restart is actually carried
//! out differs per OS and lives in [`relaunch`].
//!
//! The same stat answers the uninstall question. Dragging the app to the Trash
//! does not stop the agent: it keeps running from the trashed bundle with its
//! macOS event tap armed, which is the worst possible moment to hold one — the
//! user is about to revoke the permissions it depends on (#674, #807). Absence
//! is ambiguous for one tick (every replace unlinks before it writes), so it
//! only means "uninstalled" once it has held for [`MISSING_TICKS_UNTIL_GONE`]
//! ticks, and then the agent shuts down through its normal path, which drops
//! the hook and detaches the tap.
//!
//! Limitation: the path is resolved once via `current_exe`, which returns the
//! fully-resolved target (`/proc/self/exe` on Linux). Installs that update by
//! flipping a symlink to a new immutable payload (Nix profiles) never change
//! the resolved file, so this watcher can't see those updates; every shipped
//! channel replaces the binary in place.

use std::path::Path;
use std::time::{Duration, SystemTime};

use tokio::sync::oneshot;
use tracing::{info, warn};

mod relaunch;

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) use relaunch::replace_process;
#[cfg(target_os = "macos")]
pub(crate) use relaunch::{schedule, schedule_after_input_monitoring_grant};

use crate::shutdown::{ShutdownRequest, ShutdownRequestSender};

/// How often to stat the executable: one `metadata` call per tick — noise next
/// to the 2 s HID enumerate — while keeping the update-to-restart window short.
const PERIOD: Duration = Duration::from_secs(10);

/// What "the binary changed" means: a different size or mtime at the same
/// path. Every real update path rewrites the file, so content hashing would
/// buy nothing.
type Fingerprint = (u64, SystemTime);

/// What one stat of our own path saw.
///
/// Absence and failure are different answers, and only one of them may condemn
/// the agent: a stat that fails for any other reason — a permission change on a
/// parent directory, an I/O error, a filesystem that cannot report a
/// modification time — says nothing about whether the app is still installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sighting {
    /// The file is there, with this fingerprint.
    Seen(Fingerprint),
    /// The path confirmably does not exist.
    Absent,
    /// The stat did not answer the question.
    Unknown,
}

fn sight(path: &Path) -> Sighting {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Sighting::Absent,
        Err(_) => return Sighting::Unknown,
    };
    match meta.modified() {
        Ok(modified) => Sighting::Seen((meta.len(), modified)),
        Err(_) => Sighting::Unknown,
    }
}

/// How many consecutive ticks with nothing at our path mean the app is gone
/// rather than being replaced. At [`PERIOD`] that is 30 s of absence — far
/// longer than the unlink/write window of any install path, and short enough
/// that an uninstall does not leave an armed event tap behind for long.
const MISSING_TICKS_UNTIL_GONE: u32 = 3;

/// What one watch tick concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tick {
    /// Nothing has settled; keep watching.
    Watch,
    /// The file at our path settled on new content — restart as it.
    Restart,
    /// Our executable has been gone long enough to mean uninstalled.
    Uninstalled,
}

/// What the watcher carries between ticks.
#[derive(Debug)]
struct Watch {
    /// A changed fingerprint waiting to be confirmed by the next tick.
    pending: Option<Fingerprint>,
    /// Consecutive ticks that found nothing at our path.
    missing: u32,
    /// Whether a sustained absence may be read as an uninstall at all. False
    /// for a translocated bundle, whose mount can go away on its own.
    condemn_on_absence: bool,
}

impl Watch {
    fn new(condemn_on_absence: bool) -> Self {
        Self {
            pending: None,
            missing: 0,
            condemn_on_absence,
        }
    }

    /// Fold one observation into the watch state.
    ///
    /// A change must hold still for two consecutive ticks before it triggers a
    /// restart: a non-atomic replacement (`cp`, the linker rewriting the file in
    /// place) is observable mid-write, and exec'ing a half-written image would
    /// kill the agent instead of updating it.
    ///
    /// Absence is the ambiguous observation — mid-replace the old inode is
    /// unlinked before the new file lands — so it only becomes a verdict after
    /// [`MISSING_TICKS_UNTIL_GONE`] *consecutive* ticks of it. A
    /// [`Sighting::Unknown`] tick breaks that run rather than extending it: it
    /// could not establish that the file was missing, and in a real uninstall
    /// the following ticks are absent anyway, so starting the count over costs
    /// one tick and removes a way to shut down a live install.
    fn tick(&mut self, baseline: Fingerprint, now: Sighting) -> Tick {
        let now = match now {
            Sighting::Seen(now) => now,
            Sighting::Unknown => {
                self.missing = 0;
                return Tick::Watch;
            }
            Sighting::Absent => {
                self.pending = None;
                self.missing += 1;
                return if self.condemn_on_absence && self.missing >= MISSING_TICKS_UNTIL_GONE {
                    Tick::Uninstalled
                } else {
                    Tick::Watch
                };
            }
        };
        self.missing = 0;
        if now == baseline {
            self.pending = None;
            return Tick::Watch;
        }
        // The same non-baseline fingerprint twice in a row means the write has
        // settled; the first sighting only arms.
        let settled = self.pending == Some(now);
        self.pending = Some(now);
        if settled { Tick::Restart } else { Tick::Watch }
    }
}

/// Whether `path` sits inside a macOS App Translocation mount — the randomized,
/// read-only copy the system runs a quarantined bundle from. That path can
/// vanish for reasons that have nothing to do with an uninstall, so the
/// gone-for-good verdict stays off there. No such path exists off macOS.
fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "AppTranslocation")
}

/// Spawn the watcher thread. The executable path and its baseline fingerprint
/// are resolved once, up front; if either fails the watch is disabled (logged)
/// rather than guessing at a path.
///
/// Settled replacement and uninstall decisions are sent to the lifecycle
/// owner. This thread never replaces or exits the process itself: doing so
/// would bypass the HID++ managers that own firmware diversion.
pub fn spawn(requests: ShutdownRequestSender) {
    let Ok(path) = std::env::current_exe() else {
        warn!("could not resolve own executable — binary-update watch disabled");
        return;
    };
    let Sighting::Seen(baseline) = sight(&path) else {
        warn!(
            path = %path.display(),
            "could not stat own executable — binary-update watch disabled"
        );
        return;
    };
    // A translocated bundle already lives on a randomized, ephemeral mount, so
    // its path disappearing says nothing about whether the app is installed.
    let mut watch = Watch::new(!is_translocated(&path));
    let spawn_result = std::thread::Builder::new()
        .name("openlogi-binary-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(PERIOD);
                match watch.tick(baseline, sight(&path)) {
                    Tick::Watch => {}
                    Tick::Restart => {
                        let (retry_tx, retry_rx) = oneshot::channel();
                        if requests
                            .send(ShutdownRequest::Restart {
                                path: path.clone(),
                                retry: retry_tx,
                            })
                            .is_err()
                        {
                            return;
                        }
                        // A successful replacement ends this process. The only
                        // reply means scheduling or exec failed and this old
                        // image safely resumed its watcher fleet.
                        if retry_rx.blocking_recv().is_err() {
                            return;
                        }
                        watch.pending = None;
                    }
                    Tick::Uninstalled => {
                        info!(
                            path = %path.display(),
                            "own executable is gone — the app was removed; shutting down"
                        );
                        let _ = requests.send(ShutdownRequest::Uninstalled);
                        return;
                    }
                }
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn the binary-update watch thread");
    }
}

#[cfg(test)]
mod tests {
    use super::{Fingerprint, MISSING_TICKS_UNTIL_GONE, Sighting, Tick, Watch, is_translocated};
    use std::time::{Duration, SystemTime};

    fn fp(len: u64, secs: u64) -> Fingerprint {
        (len, SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn seen(len: u64, secs: u64) -> Sighting {
        Sighting::Seen(fp(len, secs))
    }

    #[test]
    fn restarts_only_after_a_change_settles() {
        let baseline = fp(100, 1);
        let new = fp(200, 2);
        let mut watch = Watch::new(true);
        // First differing sighting arms but does not restart…
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Watch);
        // …the same fingerprint on the next tick restarts.
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Restart);
    }

    #[test]
    fn churning_writes_keep_rearming() {
        let baseline = fp(100, 1);
        let mut watch = Watch::new(true);
        // A still-growing file never matches its previous sighting.
        assert_eq!(watch.tick(baseline, seen(150, 2)), Tick::Watch);
        assert_eq!(watch.tick(baseline, seen(200, 3)), Tick::Watch);
    }

    #[test]
    fn a_reverted_file_disarms() {
        let baseline = fp(100, 1);
        let mut watch = Watch::new(true);
        assert_eq!(watch.tick(baseline, seen(200, 2)), Tick::Watch);
        // Back at the baseline (e.g. a rollback): disarm, so a later sighting
        // of the same candidate has to settle again.
        assert_eq!(watch.tick(baseline, Sighting::Seen(baseline)), Tick::Watch);
        assert_eq!(watch.tick(baseline, seen(200, 2)), Tick::Watch);
    }

    #[test]
    fn a_brief_absence_is_a_replacement_not_an_uninstall() {
        let baseline = fp(100, 1);
        let new = fp(200, 2);
        let mut watch = Watch::new(true);
        // The unlink half of a replace, for every tick but the last one that
        // would condemn it…
        for _ in 1..MISSING_TICKS_UNTIL_GONE {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
        // …then the new file lands and settles: a restart, and the absence
        // count is forgotten.
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Watch);
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Restart);
        for _ in 1..MISSING_TICKS_UNTIL_GONE {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
    }

    #[test]
    fn a_sustained_absence_is_an_uninstall() {
        let baseline = fp(100, 1);
        let mut watch = Watch::new(true);
        for _ in 1..MISSING_TICKS_UNTIL_GONE {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
        assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Uninstalled);
    }

    #[test]
    fn an_armed_candidate_does_not_survive_the_file_going_away() {
        let baseline = fp(100, 1);
        let new = fp(200, 2);
        let mut watch = Watch::new(true);
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Watch);
        assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        // The candidate has to settle again rather than restarting off a
        // sighting from before the gap.
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Watch);
        assert_eq!(watch.tick(baseline, Sighting::Seen(new)), Tick::Restart);
    }

    #[test]
    fn a_stat_that_fails_for_any_other_reason_never_condemns() {
        let baseline = fp(100, 1);
        let mut watch = Watch::new(true);
        // Not absence: a permission change on a parent, an I/O error, a
        // filesystem with no mtime. However long it lasts, the agent stays.
        for _ in 0..MISSING_TICKS_UNTIL_GONE * 3 {
            assert_eq!(watch.tick(baseline, Sighting::Unknown), Tick::Watch);
        }
        // And it breaks a run of absences rather than extending it: the run
        // has to be consecutive, or a stat that never answers could add up to
        // a shutdown one uncertain tick at a time.
        for _ in 1..MISSING_TICKS_UNTIL_GONE {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
        assert_eq!(watch.tick(baseline, Sighting::Unknown), Tick::Watch);
        assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        // Absent from a clean start still condemns.
        for _ in 2..MISSING_TICKS_UNTIL_GONE {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
        assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Uninstalled);
    }

    #[test]
    fn a_translocated_bundle_is_never_condemned() {
        let baseline = fp(100, 1);
        let mut watch = Watch::new(false);
        for _ in 0..MISSING_TICKS_UNTIL_GONE * 3 {
            assert_eq!(watch.tick(baseline, Sighting::Absent), Tick::Watch);
        }
    }

    #[test]
    fn translocated_paths_are_recognised() {
        use std::path::Path;

        assert!(is_translocated(Path::new(
            "/private/var/folders/ab/xy/T/AppTranslocation/1E5A/d/OpenLogi.app/Contents/MacOS/openlogi-agent"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent"
        )));
    }
}

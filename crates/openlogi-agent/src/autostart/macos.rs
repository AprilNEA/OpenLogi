//! macOS: migrate the hand-written LaunchAgent plists away.
//!
//! Registration is not the agent's job here — the GUI maps `launch_at_login`
//! to an `SMAppService` registration of the app bundle's embedded
//! `Contents/Library/LaunchAgents` plist (label
//! [`openlogi_core::brand::AGENT_SERVICE_LABEL`]), which is what supervises
//! the agent: `KeepAlive = {SuccessfulExit: false}`, so a crash respawns and
//! the tray's Quit (a clean `exit(0)`) stays down.
//!
//! What remains is removing the plists earlier versions installed. A running
//! job loaded from one of those files keeps serving the current session
//! (killing a live agent for a file migration helps nobody) and disappears at
//! the next login, when its plist is no longer there to load; if the GUI has
//! registered the service meanwhile, the freshly started duplicate loses the
//! singleton lock and exits cleanly.

use std::io;
use std::path::PathBuf;

use tracing::{info, warn};

/// Labels of the hand-written `~/Library/LaunchAgents` plists earlier
/// versions installed, removed on migration: the pre-split GUI autostart and
/// the agent's own pre-`SMAppService` plist. Frozen history — each happens to
/// match a `brand::` identifier, but if a brand value ever changes these must
/// not, or the stale plists are never cleaned up. This is also why the
/// `SMAppService` label ([`openlogi_core::brand::AGENT_SERVICE_LABEL`]) is a
/// *new* name: reusing a legacy label would make "is this job the old file's
/// or ours?" unanswerable during migration.
const LEGACY_LABELS: [&str; 2] = ["org.openlogi.openlogi", "org.openlogi.agent"];

/// Migration only — `enabled` is the GUI's to act on (see the module doc).
pub(super) fn reconcile(enabled: bool) {
    let _ = enabled;
    remove_legacy();
}

/// Remove the legacy hand-written LaunchAgent plists (see [`LEGACY_LABELS`]).
/// Best-effort: a present-but-unremovable file is logged and left alone, and a
/// job currently loaded from one keeps running until logout.
fn remove_legacy() {
    for label in LEGACY_LABELS {
        let Ok(path) = plist_path(label) else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => info!("removed legacy LaunchAgent ({label})"),
            Err(e) => warn!(error = %e, label, "could not remove legacy LaunchAgent"),
        }
    }
}

fn plist_path(label: &str) -> io::Result<PathBuf> {
    let home =
        openlogi_core::paths::home_dir().map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

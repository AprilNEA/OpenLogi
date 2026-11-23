//! Accessibility permission watcher.

use std::time::Duration;

use openlogi_hook::Hook;
use tokio::sync::mpsc;

use super::poll::{self, Cause, Poll};

/// Watch macOS Accessibility permission changes.
///
/// Read for as long as a hook is installed: an active event tap that outlives
/// its grant wedges system input until reboot, so the agent has to learn about
/// a revocation on its own (see `crates/openlogi-hook/AGENTS.md`). The hook's
/// own probe cue is the safety path; this feeds the status clients see.
///
/// The full grant check (`Hook::has_accessibility`) proves the live tap
/// capability by creating a throwaway event tap, and creating a *denied* tap
/// leaks inside CoreGraphics/TCC — a probe every heartbeat grew the agent past
/// 3 GB in days (#2623). So a plain heartbeat only reads the trust bit; the
/// probe runs on start, on every privacy notification, and on the trust bit's
/// rising edge. A `false` trust read is authoritative (no tap installs without
/// it); a stale `true` after a row deletion (#674) costs only a late status
/// fix here, because every row edit posts a notification and the live tap
/// still probes on cue.
#[must_use]
pub fn spawn(heartbeat: Duration) -> mpsc::UnboundedReceiver<bool> {
    if !cfg!(target_os = "macos") {
        // Linux and Windows gate the hook below the privacy layer, so there is
        // nothing here that can change.
        return poll::constant(true);
    }
    Poll {
        name: "openlogi-accessibility-watcher",
        heartbeat,
        degrades: "the permission gate won't auto-dismiss",
    }
    .on_change({
        // The trust bit as of the last read, and the last value reported
        // downstream — the two differ only while the bit is stale-true.
        let mut trusted_seen = false;
        let mut reported = false;
        move |cause| {
            reported = match cause {
                Cause::Start | Cause::Notification => {
                    trusted_seen = Hook::accessibility_trusted();
                    Hook::has_accessibility()
                }
                Cause::Heartbeat => {
                    let trusted = Hook::accessibility_trusted();
                    let rising = trusted && !trusted_seen;
                    trusted_seen = trusted;
                    if !trusted {
                        false
                    } else if rising {
                        Hook::has_accessibility()
                    } else {
                        reported
                    }
                }
            };
            reported
        }
    })
}

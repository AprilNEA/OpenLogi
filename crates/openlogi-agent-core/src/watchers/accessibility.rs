//! Accessibility permission watcher.

use std::time::Duration;

use openlogi_hook::Hook;
use tokio::sync::mpsc;

use super::poll::{self, Poll};

/// Watch macOS Accessibility permission changes.
///
/// Read for as long as a hook is installed: an active event tap that outlives
/// its grant wedges system input until reboot, so the agent has to learn about
/// a revocation on its own (see `crates/openlogi-hook/AGENTS.md`). The hook's
/// own probe cue is the safety path; this feeds the status clients see.
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
    .on_change(Hook::has_accessibility)
}

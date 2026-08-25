//! Foreground application polling watcher.

use std::time::Duration;

use openlogi_core::app::ForegroundApp;
use tokio::sync::mpsc;

use super::poll::{self, Poll};

/// Channel item: `Some(app)` when an app is frontmost; `None` for "no
/// foreground app" (rare on macOS — Finder is usually frontmost even when
/// nothing else is).
pub type ForegroundUpdate = Option<ForegroundApp>;

/// Watch foreground application changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        // No way to read the frontmost app, so per-app profiles never switch.
        return poll::never();
    }
    Poll {
        name: "openlogi-app-watcher",
        period,
        degrades: "per-app profiles are disabled",
    }
    .on_change(openlogi_hook::frontmost_application)
}

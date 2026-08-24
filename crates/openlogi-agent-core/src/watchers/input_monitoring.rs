//! Input Monitoring permission polling watcher.

use std::time::Duration;

use tokio::sync::mpsc;

use super::poll::{self, Poll};

/// Watch macOS Input Monitoring permission changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    if !cfg!(target_os = "macos") {
        // Only macOS gates HID access behind a privacy grant.
        return poll::constant(true);
    }
    Poll {
        name: "openlogi-input-monitoring-watcher",
        period,
        degrades: "the permission status won't auto-refresh",
    }
    .on_change(openlogi_hid::permissions::has_access)
}

//! Bluetooth authorization polling watcher.

use std::time::Duration;

use tokio::sync::mpsc;

use super::poll::{self, Poll};

/// Watch macOS Bluetooth permission changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    if !cfg!(target_os = "macos") {
        return poll::constant(false);
    }
    Poll {
        name: "openlogi-bluetooth-watcher",
        period,
        degrades: "the permission status won't auto-refresh",
    }
    .on_change(openlogi_hid::permissions::has_bluetooth_access)
}

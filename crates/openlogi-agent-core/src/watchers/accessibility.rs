//! Accessibility permission polling watcher.

use std::thread;
use std::time::Duration;

use openlogi_hook::Hook;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Watch macOS Accessibility permission changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = mpsc::unbounded_channel();

    if !cfg!(target_os = "macos") {
        let _ = tx.send(true);
        let _ = period;
        return rx;
    }

    let spawn_result = thread::Builder::new()
        .name("openlogi-accessibility-watcher".into())
        .spawn(move || {
            let mut last: Option<bool> = None;
            loop {
                let granted = Hook::has_accessibility();
                if last != Some(granted) {
                    debug!(granted, "accessibility trust changed");
                    if tx.send(granted).is_err() {
                        debug!("accessibility watcher receiver dropped — exiting");
                        return;
                    }
                    last = Some(granted);
                }
                thread::sleep(period);
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn accessibility watcher — gate won't auto-dismiss");
    }
    rx
}

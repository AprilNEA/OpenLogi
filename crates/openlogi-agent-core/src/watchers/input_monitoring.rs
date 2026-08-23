//! Input Monitoring permission polling watcher.

use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Watch macOS Input Monitoring permission changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = mpsc::unbounded_channel();

    if !cfg!(target_os = "macos") {
        let _ = tx.send(true);
        let _ = period;
        return rx;
    }

    let spawn_result = thread::Builder::new()
        .name("openlogi-input-monitoring-watcher".into())
        .spawn(move || {
            let mut last: Option<bool> = None;
            loop {
                let granted = openlogi_hid::permissions::has_access();
                if last != Some(granted) {
                    debug!(granted, "input monitoring trust changed");
                    if tx.send(granted).is_err() {
                        debug!("input monitoring watcher receiver dropped — exiting");
                        return;
                    }
                    last = Some(granted);
                }
                thread::sleep(period);
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn input monitoring watcher — status won't auto-refresh");
    }
    rx
}

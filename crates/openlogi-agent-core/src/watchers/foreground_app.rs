//! Foreground application polling watcher.

use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Channel item: `Some(bundle_id)` when an app is frontmost; `None` for
/// "no foreground app" (rare on macOS — Finder is usually frontmost even
/// when nothing else is).
pub type ForegroundUpdate = Option<String>;

/// Watch foreground application changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        drop(tx);
        let _ = period;
        return rx;
    }
    let spawn_result = thread::Builder::new()
        .name("openlogi-app-watcher".into())
        .spawn(move || {
            let mut last: ForegroundUpdate = None;
            let mut first_tick = true;
            loop {
                let current = openlogi_hook::frontmost_bundle_id();
                if first_tick || current != last {
                    debug!(?current, ?last, "frontmost app changed");
                    if tx.send(current.clone()).is_err() {
                        debug!("app watcher receiver dropped — exiting");
                        return;
                    }
                    last = current;
                    first_tick = false;
                }
                thread::sleep(period);
            }
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn app watcher — per-app profiles disabled");
    }
    rx
}

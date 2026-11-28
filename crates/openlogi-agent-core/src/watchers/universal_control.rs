//! The macOS Universal Control watcher.
//!
//! While Universal Control has the pointer on another Mac or iPad, that
//! device receives this Mac's HID reports but nothing the agent posts, so a
//! diverted wheel or button would act on this Mac, where the user is not
//! looking. This samples where the pointer is and reports changes, so
//! capture can hand the mouse back to macOS while the pointer is away.

use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How often the watcher samples. The pointer-context watcher samples at the
/// same rate; one read costs about 13 µs.
pub const PERIOD: Duration = Duration::from_millis(50);

/// Start the watcher: one sample now and one every `period`. It sends the
/// first state and each change after it, `true` while the pointer is on
/// another device, and stops within one period once the receiver is dropped
/// or the state stops being readable.
///
/// Off macOS there is no Universal Control: the receiver closes without a
/// value.
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = mpsc::unbounded_channel();
    if !cfg!(target_os = "macos") {
        return rx;
    }
    let spawn_result = thread::Builder::new()
        .name("openlogi-universal-control-watcher".into())
        .spawn(move || {
            let mut last = None;
            while !tx.is_closed() {
                let Some(elsewhere) = openlogi_hook::pointer_on_other_device() else {
                    debug!("Universal Control input state unreadable — watcher exiting");
                    break;
                };
                if last != Some(elsewhere) {
                    if last.is_some() || elsewhere {
                        log_change(elsewhere);
                    }
                    if tx.send(elsewhere).is_err() {
                        break;
                    }
                    last = Some(elsewhere);
                }
                thread::sleep(period);
            }
            debug!("Universal Control watcher stopped");
        });
    if let Err(error) = spawn_result {
        warn!(error = %error, "could not spawn the Universal Control watcher");
    }
    rx
}

fn log_change(elsewhere: bool) {
    if elsewhere {
        info!("Universal Control moved the pointer to another device — leaving the mouse native");
    } else {
        info!("the pointer is back from Universal Control — capturing the mouse again");
    }
}

//! Off-tap pointer-window discovery with a coalescing publication.

use std::{thread, time::Duration};

use openlogi_hook::{PointerContext, PointerTarget};
use tokio::sync::watch;
use tracing::warn;

/// Observe the window or desktop under the pointer. Only context transitions
/// are published; pointer motion within one window does not rebuild mappings.
/// Native queries never run on the input callback, and a slow consumer retains
/// only the latest snapshot rather than an unbounded history of hover changes.
#[must_use]
pub fn spawn() -> watch::Receiver<PointerContext> {
    let supported = openlogi_hook::pointer_context_supported();
    let (tx, rx) = watch::channel(PointerContext {
        app: None,
        target: if supported {
            PointerTarget::Unavailable
        } else {
            PointerTarget::Unsupported
        },
    });
    if !supported {
        return rx;
    }
    let spawned = thread::Builder::new()
        .name("openlogi-pointer-context".into())
        .spawn(move || {
            while !tx.is_closed() {
                let current = openlogi_hook::pointer_context();
                tx.send_if_modified(|previous| {
                    if *previous == current {
                        return false;
                    }
                    *previous = current;
                    true
                });
                thread::sleep(Duration::from_millis(50));
            }
        });
    if let Err(error) = spawned {
        warn!(%error, "could not start pointer context watcher — pointer remaps remain unavailable");
    }
    rx
}

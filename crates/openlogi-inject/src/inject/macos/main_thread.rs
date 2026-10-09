//! Main-thread work asked for from the action worker.
//!
//! Text Input Source Services and AppKit's menus belong to the main thread,
//! where the agent runs `NSApplication`. A worker hands the work to the main
//! queue and waits a bounded time, so a main thread that is not running its
//! run loop costs a timeout instead of a hang.

use std::sync::mpsc;
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;

/// How long a worker waits for the main thread. The work itself takes well
/// under a millisecond; this only bounds a main thread that is not running
/// its run loop.
const MAIN_THREAD_WAIT: Duration = Duration::from_millis(250);

/// Run `work` on the main thread and return its result, or `None` when the
/// main thread did not answer in time. `what` names the work in the warning.
pub(super) fn on_main<T: Send + 'static>(
    what: &'static str,
    work: impl FnOnce(MainThreadMarker) -> T + Send + 'static,
) -> Option<T> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Some(work(mtm));
    }
    let (reply, answer) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            // A reply after the wait gave up has nobody left to read it.
            let _ = reply.send(work(mtm));
        }
    });
    answer
        .recv_timeout(MAIN_THREAD_WAIT)
        .inspect_err(|_| tracing::warn!(what, "main thread did not answer in time"))
        .ok()
}

//! "Open your main window" — an agent→GUI signal.
//!
//! The GUI is the IPC *client*, so the agent cannot call it; the GUI instead
//! long-polls this channel exactly as it does for ring presses.
//!
//! It exists because the GUI can legitimately run with **no window**: with
//! `--background` (login autostart) it never opens one, and the ring
//! overlay's always-resident hidden window keeps the process alive after the
//! user closes the main window. The Windows tray's "Show Main Window" can
//! only focus an existing window or spawn a new process — and a spawn exits
//! immediately on the singleton lock — so without this signal a windowless
//! GUI is unreachable: the tray logs "no window was found to focus" and
//! nothing opens. macOS reaches the same code path through an `openlogi://`
//! deeplink, which Windows has no scheme registration for.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

/// How long a `next` poll waits before answering "nothing yet". Bounded so a
/// disconnected client is noticed and the RPC never looks hung.
const HOLD: Duration = Duration::from_secs(20);

/// Pending "show the main window" requests, coalesced.
///
/// Cloning shares one channel. Requests are a flag rather than a queue: three
/// impatient tray clicks mean "show the window", not "show it three times".
#[derive(Clone, Default)]
pub struct ShowChannel {
    requested: Arc<Mutex<bool>>,
    notify: Arc<Notify>,
}

impl ShowChannel {
    /// Ask the GUI to open (or focus) its main window.
    pub fn request(&self) {
        *self
            .requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.notify.notify_waiters();
    }

    /// Long-poll for a request: `true` once one is pending, `false` when the
    /// hold elapses so the caller can re-poll.
    pub async fn next(&self) -> bool {
        if self.take() {
            return true;
        }
        // Register before re-checking so a request racing this wait is not
        // missed between the check and the await.
        let waiter = self.notify.notified();
        if self.take() {
            return true;
        }
        tokio::time::timeout(HOLD, waiter).await.is_ok() && self.take()
    }

    /// Consume a pending request, if any.
    fn take(&self) -> bool {
        let mut pending = self
            .requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *pending)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::ShowChannel;

    #[tokio::test]
    async fn a_pending_request_answers_immediately_and_only_once() {
        let chan = ShowChannel::default();
        chan.request();
        assert!(chan.next().await, "the queued request is delivered");
        // Second poll has nothing left; it holds, so use a paused clock.
        assert!(!chan.take(), "the request was consumed, not repeated");
    }

    #[tokio::test]
    async fn repeated_requests_coalesce_into_one() {
        let chan = ShowChannel::default();
        chan.request();
        chan.request();
        chan.request();
        assert!(chan.next().await);
        assert!(!chan.take(), "impatient clicks open one window, not three");
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_channel_answers_false_after_the_hold() {
        let chan = ShowChannel::default();
        assert!(!chan.next().await, "no request pending — poll again");
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_arriving_during_the_hold_wakes_the_poll() {
        let chan = ShowChannel::default();
        let waiter = {
            let c = chan.clone();
            tokio::spawn(async move { c.next().await })
        };
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        chan.request();
        assert!(waiter.await.expect("poll task"), "the waiter was woken");
    }
}

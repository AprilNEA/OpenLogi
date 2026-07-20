//! Shared Action Ring press channel between the gesture watcher (producer)
//! and the IPC server's long-poll (consumer).
//!
//! When the pad's effective binding is ring-shaped, a tap must reach the GUI —
//! the process that draws the on-screen ring — instantly. The GUI keeps one
//! `next_ring_press` long-poll outstanding; the watcher pushes each press here
//! and the poll answers immediately, so open latency is one IPC round trip
//! rather than a poll interval.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

use crate::ipc::RingPress;

/// How long an empty `next_ring_press` poll is held before answering `None`.
/// Mirrors the pairing long-poll: long enough that an idle GUI re-polls rarely,
/// short enough that the client's request deadline (25 s) never fires first.
const HOLD: Duration = Duration::from_secs(20);

/// Presses buffered while the GUI has no poll outstanding. Deliberately small:
/// a backlog older than this is stale input (the user tapping at a dead
/// overlay), and replaying it would fire ghost open/select transitions.
const QUEUE_CAP: usize = 4;

#[derive(Default)]
struct RingChannelState {
    /// Whether the active device's effective Action Ring binding is
    /// ring-shaped. Published by the orchestrator on every rebuild; the
    /// gesture watcher routes a pad press here only while set, and to the
    /// ordinary single-action dispatch otherwise.
    armed: bool,
    /// Monotonic press counter for [`RingPress::seq`].
    seq: u64,
    pending: VecDeque<RingPress>,
}

/// Shared press channel; cheap to clone (two `Arc`s).
#[derive(Clone, Default)]
pub struct RingChannel {
    state: Arc<Mutex<RingChannelState>>,
    notify: Arc<Notify>,
}

impl RingChannel {
    fn lock(&self) -> std::sync::MutexGuard<'_, RingChannelState> {
        // Recover the guard even if a prior holder panicked — every critical
        // section below is panic-free, so the data stays consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publish whether the pad's effective binding opens the ring.
    pub fn set_armed(&self, armed: bool) {
        self.lock().armed = armed;
    }

    /// Whether a pad press should be routed to the ring overlay.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.lock().armed
    }

    /// Queue one pad press and wake the long-poll. Oldest press is dropped
    /// when the queue is full — see [`QUEUE_CAP`].
    pub fn push_press(&self) {
        {
            let mut st = self.lock();
            st.seq += 1;
            let seq = st.seq;
            if st.pending.len() == QUEUE_CAP {
                st.pending.pop_front();
            }
            st.pending.push_back(RingPress { seq });
        }
        // notify_one stores a permit when no poll is waiting, so a press that
        // lands between a poll's queue check and its await is never lost.
        self.notify.notify_one();
    }

    /// Long-poll: the oldest queued press, or `None` once [`HOLD`] elapses.
    pub async fn next_press(&self) -> Option<RingPress> {
        let deadline = tokio::time::Instant::now() + HOLD;
        loop {
            // Register interest before checking, so a push between the check
            // and the await wakes the `notified` future instead of racing it.
            let notified = self.notify.notified();
            if let Some(press) = self.lock().pending.pop_front() {
                return Some(press);
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_press_answers_immediately() {
        let chan = RingChannel::default();
        chan.push_press();
        assert_eq!(chan.next_press().await, Some(RingPress { seq: 1 }));
    }

    #[tokio::test]
    async fn presses_deliver_in_order_with_monotonic_seq() {
        let chan = RingChannel::default();
        chan.push_press();
        chan.push_press();
        assert_eq!(chan.next_press().await, Some(RingPress { seq: 1 }));
        assert_eq!(chan.next_press().await, Some(RingPress { seq: 2 }));
    }

    #[tokio::test]
    async fn overflow_drops_the_oldest_press_not_the_newest() {
        let chan = RingChannel::default();
        for _ in 0..QUEUE_CAP + 2 {
            chan.push_press();
        }
        // Seqs 1 and 2 were evicted; the survivors are the most recent CAP.
        assert_eq!(chan.next_press().await, Some(RingPress { seq: 3 }));
    }

    #[tokio::test(start_paused = true)]
    async fn empty_poll_times_out_with_none() {
        let chan = RingChannel::default();
        // Paused time: the sleep_until elapses instantly once polled.
        assert_eq!(chan.next_press().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn press_during_a_held_poll_wakes_it() {
        let chan = RingChannel::default();
        let waiter = tokio::spawn({
            let chan = chan.clone();
            async move { chan.next_press().await }
        });
        // Let the poll reach its await before pushing.
        tokio::task::yield_now().await;
        chan.push_press();
        assert_eq!(waiter.await.expect("join"), Some(RingPress { seq: 1 }));
    }

    #[tokio::test]
    async fn armed_flag_round_trips() {
        let chan = RingChannel::default();
        assert!(!chan.is_armed(), "unarmed until the orchestrator publishes");
        chan.set_armed(true);
        assert!(chan.is_armed());
        chan.set_armed(false);
        assert!(!chan.is_armed());
    }
}

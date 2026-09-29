//! Per-keyboard ordering for Fn-lock writes.
//!
//! A keyboard's Fn-lock is written from three places: the GUI toggle's RPC, a
//! config reload that changed the value, and the reconnect reapply. They share
//! the receiver lease, which orders nothing, so after two quick toggles the
//! older value could reach the keyboard last. Each write takes an
//! [`FnLockTicket`] when it is requested and writes only if it is still the
//! newest request once the keyboard's earlier writes have finished.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_hid::DeviceRoute;
use tokio::sync::MutexGuard;

/// The Fn-lock write queues, one per keyboard route.
#[derive(Clone, Default)]
pub(crate) struct FnLockOrder(Arc<Mutex<HashMap<String, Arc<Queue>>>>);

#[derive(Default)]
struct Queue {
    latest: AtomicU64,
    turn: tokio::sync::Mutex<()>,
}

/// One requested Fn-lock write, numbered in request order.
pub(crate) struct FnLockTicket {
    queue: Arc<Queue>,
    number: u64,
}

impl FnLockOrder {
    /// Number a new Fn-lock write for `route`. Call it when the write is
    /// requested, not when it runs: the number is what orders it.
    pub(crate) fn request(&self, route: &DeviceRoute) -> FnLockTicket {
        let queue = {
            // The map only holds `Arc`s, so a panic mid-insert leaves nothing
            // half-written worth refusing to read.
            let mut queues = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(queues.entry(route.to_string()).or_default())
        };
        let number = queue.latest.fetch_add(1, Ordering::AcqRel) + 1;
        FnLockTicket { queue, number }
    }
}

impl FnLockTicket {
    /// Wait for the keyboard's earlier Fn-lock writes, then hold its turn.
    /// `None` when a newer write was requested meanwhile: that write leaves
    /// the final state, so this one must not touch the keyboard.
    pub(crate) async fn turn(&self) -> Option<MutexGuard<'_, ()>> {
        let turn = self.queue.turn.lock().await;
        (self.queue.latest.load(Ordering::Acquire) == self.number).then_some(turn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(slot: u8) -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "receiver".into(),
            slot,
        }
    }

    #[tokio::test]
    async fn an_older_write_yields_to_a_newer_request() {
        let queues = FnLockOrder::default();
        let older = queues.request(&route(1));
        let newer = queues.request(&route(1));

        assert!(older.turn().await.is_none());
        assert!(newer.turn().await.is_some());
    }

    #[tokio::test]
    async fn a_write_requested_while_another_runs_waits_its_turn() {
        let queues = FnLockOrder::default();
        let running = queues.request(&route(1));
        let turn = running.turn().await.expect("newest write runs");
        let next = queues.request(&route(1));

        let waiting = tokio::spawn(async move { next.turn().await.is_some() });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(turn);
        assert!(waiting.await.expect("task joins"));
    }

    #[tokio::test]
    async fn keyboards_do_not_order_each_other() {
        let queues = FnLockOrder::default();
        let first = queues.request(&route(1));
        let _other = queues.request(&route(2));

        assert!(first.turn().await.is_some());
    }
}

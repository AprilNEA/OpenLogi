//! Per-device ordering for the writes of one setting.
//!
//! A keyboard's Fn-lock is written from three places: the GUI toggle's RPC, a
//! config reload that changed the value, and the reconnect reapply. A mouse's
//! report rate is written from two: the GUI pick's RPC and the reconnect
//! reapply. The writers share the receiver lease, which orders nothing, so
//! the older value could reach the device last — after two quick toggles, or
//! when a reapply scheduled with the configured rate lands after the user's
//! newer pick. Each write takes a [`WriteTicket`] when it is requested and
//! writes only if it is still the newest request once the device's earlier
//! writes have finished.
//!
//! A keyboard's backlight needs every write, not just the newest: each change
//! is a read-modify-write of its own field, so a battery-saving click followed
//! by a level click must both land, in that order, and two of them running at
//! once could each read the old config and write back over the other.
//! [`WriteTicket::in_order`] runs every ticket, strictly in request order.
//!
//! One [`WriteOrder`] orders one setting: Fn-lock and report-rate writes do
//! not wait for each other. A setting uses one of the two disciplines.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_hid::DeviceRoute;
use tokio::sync::{MutexGuard, Notify};

/// One setting's write queues, one per device route.
#[derive(Clone, Default)]
pub(crate) struct WriteOrder(Arc<Mutex<HashMap<String, Arc<Queue>>>>);

#[derive(Default)]
struct Queue {
    latest: AtomicU64,
    turn: tokio::sync::Mutex<()>,
    /// Which tickets are done, for [`WriteTicket::in_order`].
    finished: Mutex<Finished>,
    /// Woken whenever a ticket finishes.
    advanced: Notify,
}

/// Every ticket up to `through` is done, as are the later `early` ones,
/// which finished (were dropped) before an earlier ticket did.
#[derive(Default)]
struct Finished {
    through: u64,
    early: BTreeSet<u64>,
}

impl Queue {
    fn finish(&self, number: u64) {
        {
            // Two integers and a set of them: a panic mid-update leaves at
            // worst a ticket counted late, never a turn handed out twice.
            let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
            if number == finished.through + 1 {
                finished.through = number;
                loop {
                    let next = finished.through + 1;
                    if !finished.early.remove(&next) {
                        break;
                    }
                    finished.through = next;
                }
            } else {
                finished.early.insert(number);
            }
        }
        self.advanced.notify_waiters();
    }

    fn is_turn_of(&self, number: u64) -> bool {
        let finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        finished.through + 1 >= number
    }
}

/// One requested write, numbered in request order. Dropping it — once its
/// write ran, or when the request gave up before — lets the next one go.
pub(crate) struct WriteTicket {
    queue: Arc<Queue>,
    number: u64,
}

impl WriteOrder {
    /// Number a new write for `route`. Call it when the write is requested,
    /// not when it runs: the number is what orders it.
    pub(crate) fn request(&self, route: &DeviceRoute) -> WriteTicket {
        let queue = {
            // The map only holds `Arc`s, so a panic mid-insert leaves nothing
            // half-written worth refusing to read.
            let mut queues = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(queues.entry(route.to_string()).or_default())
        };
        let number = queue.latest.fetch_add(1, Ordering::AcqRel) + 1;
        WriteTicket { queue, number }
    }
}

impl WriteTicket {
    /// Wait for the device's earlier writes, then hold its turn. `None` when
    /// a newer write was requested meanwhile: that write leaves the final
    /// state, so this one must not touch the device.
    async fn turn(&self) -> Option<MutexGuard<'_, ()>> {
        let turn = self.queue.turn.lock().await;
        (self.queue.latest.load(Ordering::Acquire) == self.number).then_some(turn)
    }

    /// Run `write` in this ticket's turn, or `superseded` instead once a
    /// newer write was requested: a caller that owes an answer reads the
    /// device there, and a background write simply stops.
    pub(crate) async fn write_or<T, Write, Superseded>(
        &self,
        write: impl FnOnce() -> Write,
        superseded: impl FnOnce() -> Superseded,
    ) -> T
    where
        Write: Future<Output = T>,
        Superseded: Future<Output = T>,
    {
        match self.turn().await {
            Some(_turn) => write().await,
            None => superseded().await,
        }
    }

    /// Run `write` once every write requested before this one has finished,
    /// even when newer writes were requested meanwhile: unlike
    /// [`Self::write_or`], no write is skipped. The next ticket's write
    /// starts when this one returns.
    pub(crate) async fn in_order<T, Write>(self, write: impl FnOnce() -> Write) -> T
    where
        Write: Future<Output = T>,
    {
        loop {
            let advanced = self.queue.advanced.notified();
            let mut advanced = std::pin::pin!(advanced);
            // Registered before the check, so a ticket finishing between the
            // check and the wait still wakes this one.
            advanced.as_mut().enable();
            if self.queue.is_turn_of(self.number) {
                break;
            }
            advanced.await;
        }
        write().await
    }
}

impl Drop for WriteTicket {
    fn drop(&mut self) {
        self.queue.finish(self.number);
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
        let queues = WriteOrder::default();
        let older = queues.request(&route(1));
        let newer = queues.request(&route(1));

        assert!(older.turn().await.is_none());
        assert!(newer.turn().await.is_some());
    }

    #[tokio::test]
    async fn a_write_requested_while_another_runs_waits_its_turn() {
        let queues = WriteOrder::default();
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
    async fn devices_do_not_order_each_other() {
        let queues = WriteOrder::default();
        let first = queues.request(&route(1));
        let _other = queues.request(&route(2));

        assert!(first.turn().await.is_some());
    }

    /// The reconnect reapply numbers its write when it is scheduled. A pick
    /// the user makes before the reapply reaches the mouse is newer, so the
    /// reapply leaves the rate to it and the pick writes.
    #[tokio::test]
    async fn a_superseded_write_takes_the_superseded_path() {
        let rates = WriteOrder::default();
        let reapply = rates.request(&route(1));
        let pick = rates.request(&route(1));

        let reapplied = reapply
            .write_or(|| async { Some(1000) }, || async { None })
            .await;
        let picked = pick
            .write_or(|| async { Some(2000) }, || async { None })
            .await;

        assert_eq!(reapplied, None);
        assert_eq!(picked, Some(2000));
    }

    /// A pick made while the reapply is already writing waits for it, then
    /// writes last.
    #[tokio::test]
    async fn a_write_requested_mid_write_lands_after_it() {
        let rates = WriteOrder::default();
        let reapply = rates.request(&route(1));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (started, has_started) = tokio::sync::oneshot::channel::<()>();
        let writes = Arc::new(Mutex::new(Vec::new()));

        let reapply_writes = Arc::clone(&writes);
        let reapplying = tokio::spawn(async move {
            reapply
                .write_or(
                    || async move {
                        let _ = started.send(());
                        let _ = released.await;
                        reapply_writes.lock().unwrap().push(1000);
                    },
                    || async {},
                )
                .await;
        });
        has_started.await.expect("the reapply writes");

        let pick = rates.request(&route(1));
        let pick_writes = Arc::clone(&writes);
        let picking = tokio::spawn(async move {
            pick.write_or(
                || async move { pick_writes.lock().unwrap().push(2000) },
                || async {},
            )
            .await;
        });
        tokio::task::yield_now().await;
        assert!(
            !picking.is_finished(),
            "the pick waits for the running write"
        );

        release.send(()).expect("the reapply is waiting");
        reapplying.await.expect("reapply joins");
        picking.await.expect("pick joins");
        assert_eq!(*writes.lock().unwrap(), [1000, 2000]);
    }

    /// Three quick backlight changes, spawned newest first, still all run —
    /// none is skipped as superseded — oldest first and one at a time.
    #[tokio::test]
    async fn in_order_runs_every_write_one_at_a_time_in_request_order() {
        let backlight = WriteOrder::default();
        let tickets: Vec<_> = (0..3).map(|_| backlight.request(&route(1))).collect();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut writes = Vec::new();
        for (index, ticket) in tickets.into_iter().enumerate().rev() {
            let log = Arc::clone(&log);
            writes.push(tokio::spawn(ticket.in_order(move || async move {
                log.lock().unwrap().push(format!("start {index}"));
                tokio::task::yield_now().await;
                log.lock().unwrap().push(format!("end {index}"));
            })));
        }
        for write in writes {
            write.await.expect("write joins");
        }
        assert_eq!(
            *log.lock().unwrap(),
            ["start 0", "end 0", "start 1", "end 1", "start 2", "end 2"]
        );
    }

    /// A request that gave up — cancelled, or refused before its turn —
    /// drops its ticket. The writes behind it still run, even when it gave up
    /// before an earlier write had finished.
    #[tokio::test]
    async fn a_dropped_ticket_does_not_hold_up_the_writes_behind_it() {
        let backlight = WriteOrder::default();
        let first = backlight.request(&route(1));
        let abandoned = backlight.request(&route(1));
        let last = backlight.request(&route(1));
        drop(abandoned);
        let ran = Arc::new(Mutex::new(Vec::new()));
        let last_ran = Arc::clone(&ran);
        let waiting = tokio::spawn(last.in_order(move || async move {
            last_ran.lock().unwrap().push(3);
        }));
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "the last write waits for the first");
        first
            .in_order(|| async { ran.lock().unwrap().push(1) })
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("the dropped ticket does not block the last write")
            .expect("the last write joins");
        assert_eq!(*ran.lock().unwrap(), [1, 3]);
    }
}

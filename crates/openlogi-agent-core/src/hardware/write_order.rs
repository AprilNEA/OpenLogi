//! Per-route setting intents, shared by foreground and background writers.
//!
//! A receiver lease is not a write queue. Hold a ticket's turn through readback;
//! retries reuse their policy ticket instead of becoming a new user intent.
//!
//! Independent settings choose one discipline: newest-request wins for Fn-lock
//! and report rate; strict request order for backlight read-modify-writes and
//! host renames; confirmed-policy ordering for Disable Keys. The latter keeps
//! old retries behind manual choices and restores accepted policy when a newer
//! request fails or is cancelled. All share the same per-route turn owner.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use openlogi_hid::DeviceRoute;

/// Independent instances isolate settings; routes isolate keyboards.
#[derive(Clone, Default)]
pub(crate) struct WriteOrder<T = ()>(Arc<Mutex<HashMap<String, Arc<Queue<T>>>>>);

#[derive(Default)]
struct Queue<T> {
    state: Mutex<State<T>>,
    turn: tokio::sync::Mutex<()>,
    finished: Mutex<Finished>,
    advanced: tokio::sync::Notify,
}

#[derive(Default)]
struct State<T> {
    next: u64,
    committed: Option<Intent<T>>,
    policy: Option<Intent<T>>,
    pending: BTreeSet<u64>,
    recovery: Recovery,
}

#[derive(Default)]
enum Recovery {
    #[default]
    None,
    Requested,
    Scheduled,
}

#[derive(Clone)]
struct Intent<T> {
    number: u64,
    value: T,
}

impl<T> Queue<T> {
    fn state(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Every ticket up to `through` is done, as are the later `early` ones,
/// which finished (were dropped) before an earlier ticket did.
#[derive(Default)]
struct Finished {
    issued: u64,
    through: u64,
    early: BTreeSet<u64>,
}

impl<T> Queue<T> {
    fn reserve(self: &Arc<Self>) -> Arc<ReservedTurn<T>> {
        let number = {
            let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
            finished.issued += 1;
            finished.issued
        };
        Arc::new(ReservedTurn {
            queue: Arc::clone(self),
            number,
        })
    }

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

impl<T> State<T> {
    fn intent(&mut self, value: T) -> Intent<T> {
        self.next += 1;
        Intent {
            number: self.next,
            value,
        }
    }

    fn latest(&self) -> u64 {
        self.pending
            .last()
            .copied()
            .unwrap_or_default()
            .max(self.committed.as_ref().map_or(0, |intent| intent.number))
    }
}

/// One intent; cloning it creates another attempt, not another intent.
#[derive(Clone)]
pub(crate) struct WriteTicket<T = ()> {
    queue: Arc<Queue<T>>,
    number: u64,
    // Only request-ordered writes retire a sequence slot; policy retries reuse an intent.
    completion: Option<Arc<ReservedTurn<T>>>,
}

impl<T: Clone + Default + Eq> WriteOrder<T> {
    /// Reserve I/O order without publishing a new setting intent. Reads use
    /// this to wait for policy workers already queued by a config reload.
    pub(crate) fn reserve(&self, route: &DeviceRoute) -> Arc<ReservedTurn<T>> {
        self.queue(route).reserve()
    }

    fn queue(&self, route: &DeviceRoute) -> Arc<Queue<T>> {
        Arc::clone(
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(route.to_string())
                .or_default(),
        )
    }

    /// Immediately publish a new intent for request-ordered settings.
    fn request_value(&self, route: &DeviceRoute, value: T) -> WriteTicket<T> {
        let queue = self.queue(route);
        let number = {
            let mut state = queue.state();
            let intent = state.intent(value);
            let number = intent.number;
            state.committed = Some(intent);
            number
        };
        WriteTicket {
            queue,
            number,
            completion: None,
        }
    }

    /// Publish a changed policy, or acknowledge a matching confirmed RPC.
    /// An unchanged, different old policy must never supersede an unsaved RPC.
    pub(crate) fn sync_policy(&self, route: &DeviceRoute, value: T) -> bool {
        let queue = self.queue(route);
        let mut state = queue.state();
        if state
            .committed
            .as_ref()
            .is_some_and(|intent| intent.value == value)
        {
            let State {
                policy, committed, ..
            } = &mut *state;
            policy.clone_from(committed);
            return false;
        }
        if state
            .policy
            .as_ref()
            .is_some_and(|policy| policy.value == value)
        {
            return false;
        }
        if state.policy.is_none() {
            // Inventory may first publish config while an RPC already owns
            // this route. The baseline predates every interactive intent.
            let intent = Intent { number: 0, value };
            if state.committed.is_none() {
                state.committed = Some(intent.clone());
            }
            state.policy = Some(intent);
            return true;
        }
        let intent = state.intent(value);
        state.committed = Some(intent.clone());
        state.policy = Some(intent);
        state.recovery = Recovery::None;
        true
    }

    /// Retirement differs from a value transition: even an unmanaged route
    /// can have a queued manual request. Retire it before forgetting the route.
    pub(crate) fn retire(&self, route: &DeviceRoute) {
        let queue = self.queue(route);
        let mut state = queue.state();
        let intent = state.intent(T::default());
        state.committed = Some(intent.clone());
        state.policy = Some(intent);
        state.recovery = Recovery::None;
    }

    /// Reuse saved policy, or the latest accepted intent during an explicitly
    /// scheduled recovery. Neither path gives a retry new write authority.
    pub(crate) fn policy(&self, route: &DeviceRoute) -> Option<(T, WriteTicket<T>)> {
        let queue = self.queue(route);
        let intent = {
            let state = queue.state();
            if matches!(state.recovery, Recovery::Scheduled) {
                // A confirmed RPC can precede its config save. Recovery must
                // restore that accepted choice, not the older saved value.
                state.committed.clone()
            } else {
                state.policy.clone()
            }
        }?;
        Some((
            intent.value,
            WriteTicket {
                queue,
                number: intent.number,
                completion: None,
            },
        ))
    }

    /// Hand abandoned-request recovery to the existing bounded scheduler.
    /// A pending successor keeps ownership; its completion will wake inventory.
    pub(crate) fn schedule_recovery(&self, route: &DeviceRoute) -> bool {
        let queue = self.queue(route);
        let mut state = queue.state();
        if !state.pending.is_empty() || !matches!(state.recovery, Recovery::Requested) {
            return false;
        }
        state.recovery = Recovery::Scheduled;
        true
    }

    /// The scheduled run has spent its last attempt. Tickets already handed
    /// to writers retain their accepted identity even after this transition.
    pub(crate) fn finish_recovery(&self, route: &DeviceRoute) {
        let queue = self.queue(route);
        let mut state = queue.state();
        if matches!(state.recovery, Recovery::Scheduled) {
            state.recovery = Recovery::None;
        }
    }

    /// A pending RPC owns priority until confirmation or cancellation/failure.
    pub(crate) fn begin(&self, route: &DeviceRoute, value: T) -> PendingWrite<T> {
        let queue = self.queue(route);
        let intent = {
            let mut state = queue.state();
            let intent = state.intent(value);
            state.pending.insert(intent.number);
            intent
        };
        PendingWrite {
            ticket: WriteTicket {
                queue,
                number: intent.number,
                completion: None,
            },
            intent,
            abandoned: None,
        }
    }
}

impl<T> WriteTicket<T> {
    /// Reserve before dispatching a worker, not when its thread gets polled.
    pub(crate) fn reserve(&self) -> Arc<ReservedTurn<T>> {
        self.queue.reserve()
    }

    /// Earlier writes finish before a current ticket can start HID I/O.
    #[cfg(test)]
    pub(crate) async fn turn(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let turn = self.queue.turn.lock().await;
        self.is_current().then_some(turn)
    }

    pub(crate) fn is_current(&self) -> bool {
        self.queue.state().latest() == self.number
    }
}

impl<V> WriteTicket<V> {
    /// Acquire the setting turn and the caller's I/O guards without starting
    /// I/O. Cancellation may drop this wait; an admitted writer must keep the
    /// returned guards through native completion and recheck its intent.
    pub(crate) async fn acquire<Guard>(
        &self,
        ready: impl Future<Output = Guard>,
    ) -> (tokio::sync::MutexGuard<'_, ()>, Guard) {
        let turn = self.queue.turn.lock().await;
        (turn, ready.await)
    }

    /// Run `write` in this ticket's turn, or `superseded` instead once a
    /// newer write was requested: a caller that owes an answer reads the
    /// device there, and a background write simply stops. Acquire the
    /// transport guard from `ready` before deciding: an intent can be retired
    /// while waiting for another setting's I/O. Hold both guards through I/O.
    pub(crate) async fn write_or<T, Guard, Write, Superseded>(
        &self,
        ready: impl Future<Output = Guard>,
        write: impl FnOnce() -> Write,
        superseded: impl FnOnce() -> Superseded,
    ) -> T
    where
        Write: Future<Output = T>,
        Superseded: Future<Output = T>,
    {
        let (_turn, _ready) = self.acquire(ready).await;
        if self.is_current() {
            write().await
        } else {
            superseded().await
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
        if let Some(turn) = &self.completion {
            turn.wait().await;
        }
        write().await
    }
}

/// Completes a strictly ordered slot only when its final handle is dropped.
pub(crate) struct ReservedTurn<T> {
    queue: Arc<Queue<T>>,
    number: u64,
}

impl<T> ReservedTurn<T> {
    /// Wait until all earlier reservations have drained or been dropped.
    /// Hold this reservation until the operation, including native I/O, ends.
    pub(crate) async fn wait(&self) {
        loop {
            let advanced = self.queue.advanced.notified();
            let mut advanced = std::pin::pin!(advanced);
            advanced.as_mut().enable();
            if self.queue.is_turn_of(self.number) {
                return;
            }
            advanced.await;
        }
    }
}

impl<T> Drop for ReservedTurn<T> {
    fn drop(&mut self) {
        self.queue.finish(self.number);
    }
}

impl WriteOrder {
    /// Number a new request. Existing settings use request ordering; retries of
    /// persisted policy use `sync_policy` / `policy` instead.
    pub(crate) fn request(&self, route: &DeviceRoute) -> WriteTicket {
        let ticket = self.request_value(route, ());
        WriteTicket {
            completion: Some(ticket.reserve()),
            ..ticket
        }
    }
}

/// RAII prevents failed/cancelled RPCs from permanently retiring saved policy.
pub(crate) struct PendingWrite<T> {
    ticket: WriteTicket<T>,
    intent: Intent<T>,
    abandoned: Option<Box<dyn FnOnce() + Send>>,
}

impl<T: Clone> PendingWrite<T> {
    /// Notify the runtime after unsuccessful ownership release, including
    /// cancellation. The callback schedules work and must not perform HID I/O.
    pub(crate) fn on_abandoned(mut self, notify: impl FnOnce() + Send + 'static) -> Self {
        self.abandoned = Some(Box::new(notify));
        self
    }

    pub(crate) fn ticket(&self) -> WriteTicket<T> {
        self.ticket.clone()
    }

    /// Commit only a current result that the caller can return as success.
    /// A superseded completion must not strand saved policy if its successor
    /// fails: the client never accepted or saved that intermediate value.
    pub(crate) fn confirm(self) -> bool {
        let mut state = self.ticket.queue.state();
        if state.latest() != self.intent.number {
            return false;
        }
        state.committed = Some(self.intent.clone());
        state.recovery = Recovery::None;
        true
    }
}

impl<T> Drop for PendingWrite<T> {
    fn drop(&mut self) {
        let abandoned = {
            let mut state = self.ticket.queue.state();
            state.pending.remove(&self.intent.number);
            let abandoned = state
                .committed
                .as_ref()
                .is_none_or(|accepted| accepted.number < self.intent.number);
            if abandoned {
                state.recovery = Recovery::Requested;
            }
            abandoned
        };
        if abandoned && let Some(notify) = self.abandoned.take() {
            notify();
        }
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
    async fn read_reservations_wait_for_dispatched_writes_without_new_intent() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(false));
        let (_, ticket) = order.policy(&route(1)).unwrap();
        let dispatched = ticket.reserve();
        // The worker has not polled yet, but its place is already published.
        let reader = order.reserve(&route(1));
        assert!(
            futures_lite::future::poll_once(reader.wait())
                .await
                .is_none()
        );
        let other = order.reserve(&route(2));
        assert!(
            futures_lite::future::poll_once(other.wait())
                .await
                .is_some()
        );
        assert!(ticket.is_current(), "a read must not become a write intent");
        drop(dispatched);
        reader.wait().await;
        assert!(ticket.is_current());
    }

    #[tokio::test]
    async fn cancelled_reservations_do_not_strand_later_reads_or_writes() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(false));
        let first = order.reserve(&route(1));
        let cancelled = order.reserve(&route(1));
        let pending = order.begin(&route(1), Some(true));
        let writer = pending.ticket().reserve();
        let last = order.reserve(&route(1));
        drop(cancelled);
        assert!(
            futures_lite::future::poll_once(writer.wait())
                .await
                .is_none()
        );
        drop(first);
        writer.wait().await;
        assert!(futures_lite::future::poll_once(last.wait()).await.is_none());
        drop(writer);
        last.wait().await;
        assert!(pending.confirm());
    }

    #[tokio::test]
    async fn an_older_write_yields_to_a_newer_request() {
        let order = WriteOrder::default();
        let stale = order.request_value(&route(1), false);
        let newer = order.request_value(&route(1), true);
        assert!(stale.turn().await.is_none());
        assert!(newer.turn().await.is_some());
    }

    #[tokio::test]
    async fn a_write_requested_while_another_runs_waits_its_turn() {
        let order = WriteOrder::default();
        let running = order.request_value(&route(1), false);
        let turn = running.turn().await.expect("current");
        let next = order.request_value(&route(1), true);
        let mut waiting = std::pin::pin!(next.turn());
        assert!(
            futures_lite::future::poll_once(&mut waiting)
                .await
                .is_none()
        );
        drop(turn);
        assert!(waiting.await.is_some());
    }

    #[tokio::test]
    async fn keyboards_and_settings_do_not_order_each_other() {
        let order = WriteOrder::default();
        let other_setting = WriteOrder::default();
        let first = order.request_value(&route(1), false);
        let _other = order.request_value(&route(2), true);
        let _other_setting = other_setting.request_value(&route(1), true);
        assert!(first.turn().await.is_some());
    }

    #[tokio::test]
    async fn retries_before_reload_keep_the_old_policy_identity() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(1));
        let (_, old) = order.policy(&route(1)).expect("policy");
        let manual = order.begin(&route(1), Some(0));
        assert!(old.turn().await.is_none());
        assert!(manual.confirm());
        assert!(!order.sync_policy(&route(1), Some(1)));
        let (_, retry) = order.policy(&route(1)).expect("policy");
        assert!(retry.turn().await.is_none());
        order.sync_policy(&route(1), Some(0));
        assert!(
            order
                .policy(&route(1))
                .expect("policy")
                .1
                .turn()
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn same_value_save_acknowledges_without_a_new_write_intent() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(1));
        let manual = order.begin(&route(1), Some(1));
        let ticket = manual.ticket();
        assert!(manual.confirm());
        assert!(!order.sync_policy(&route(1), Some(1)));
        assert!(ticket.is_current());
        assert!(
            order
                .policy(&route(1))
                .expect("policy")
                .1
                .turn()
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn overlapping_failures_and_cancellations_restore_only_committed_policy() {
        for reverse in [false, true] {
            let order = WriteOrder::default();
            order.sync_policy(&route(1), Some(1));
            let (_, policy) = order.policy(&route(1)).expect("policy");
            let first = order.begin(&route(1), Some(0));
            let second = order.begin(&route(1), Some(2));
            assert!(!policy.is_current());
            if reverse {
                drop(second);
                drop(first);
            } else {
                drop(first);
                drop(second);
            }
            assert!(policy.turn().await.is_some());
        }
    }

    #[tokio::test]
    async fn recovery_waits_for_all_pending_requests_and_keeps_the_committed_choice() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(1));
        assert!(order.begin(&route(1), Some(2)).confirm());
        let first = order.begin(&route(1), Some(3));
        let newer = order.begin(&route(1), Some(4));
        drop(first);
        assert!(
            !order.schedule_recovery(&route(1)),
            "successor still owns priority"
        );
        order.finish_recovery(&route(1));
        drop(newer);
        assert!(order.schedule_recovery(&route(1)));
        assert!(
            !order.schedule_recovery(&route(1)),
            "one bounded scheduler run"
        );
        let (value, ticket) = order.policy(&route(1)).expect("accepted value");
        assert_eq!(value, Some(2));
        order.finish_recovery(&route(1));
        assert!(
            ticket.turn().await.is_some(),
            "last scheduled attempt retains identity"
        );
        assert!(
            !order.schedule_recovery(&route(1)),
            "budget exhaustion is final"
        );
    }

    #[test]
    fn confirmed_or_retired_requests_do_not_signal_abandoned_recovery() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let order = WriteOrder::default();
        let signals = Arc::new(AtomicUsize::new(0));
        for confirmed in [false, true] {
            let observed = Arc::clone(&signals);
            let pending = order.begin(&route(1), Some(1)).on_abandoned(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            });
            if confirmed {
                assert!(pending.confirm());
            } else {
                order.retire(&route(1));
                drop(pending);
            }
        }
        assert_eq!(signals.load(Ordering::SeqCst), 0);
        assert!(!order.schedule_recovery(&route(1)));
    }

    #[tokio::test]
    async fn superseded_confirmation_preserves_the_last_accepted_intent() {
        for previous_manual in [false, true] {
            let order = WriteOrder::default();
            order.sync_policy(&route(1), Some(1));
            let (_, mut accepted) = order.policy(&route(1)).expect("saved policy");
            if previous_manual {
                let confirmed = order.begin(&route(1), Some(2));
                accepted = confirmed.ticket();
                assert!(confirmed.confirm());
            }
            let first = order.begin(&route(1), Some(0));
            let second = order.begin(&route(1), Some(3));
            assert!(!first.confirm());
            drop(second);
            assert!(accepted.turn().await.is_some());
        }
    }

    #[tokio::test]
    async fn acknowledging_a_save_does_not_supersede_the_next_pending_choice() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(1));
        assert!(order.begin(&route(1), Some(2)).confirm());
        let next = order.begin(&route(1), Some(3));
        assert!(!order.sync_policy(&route(1), Some(2)));
        assert!(next.ticket().is_current());
        assert!(
            order
                .policy(&route(1))
                .expect("policy")
                .1
                .turn()
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn initial_policy_is_older_than_a_manual_request() {
        let order = WriteOrder::default();
        let manual = order.begin(&route(1), Some(2));
        order.sync_policy(&route(1), Some(1));
        assert!(manual.ticket().is_current());
        drop(manual);
        assert!(
            order
                .policy(&route(1))
                .expect("policy")
                .1
                .turn()
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn stale_completion_and_cleanup_cannot_replace_successors() {
        let order = WriteOrder::default();
        order.sync_policy(&route(1), Some(1));
        let stale = order.begin(&route(1), Some(0));
        let newer = order.begin(&route(1), Some(2));
        let ticket = newer.ticket();
        assert!(newer.confirm());
        assert!(!stale.confirm());
        assert!(ticket.is_current());
        let cancelled = order.begin(&route(1), Some(3));
        order.sync_policy(&route(1), None);
        drop(cancelled);
        assert!(!ticket.is_current());
        assert!(
            order
                .policy(&route(1))
                .expect("unmanaged")
                .1
                .turn()
                .await
                .is_some()
        );
    }
}

#[cfg(test)]
mod sequence_tests {
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
            .write_or(
                std::future::ready(()),
                || async { Some(1000) },
                || async { None },
            )
            .await;
        let picked = pick
            .write_or(
                std::future::ready(()),
                || async { Some(2000) },
                || async { None },
            )
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
                    std::future::ready(()),
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
                std::future::ready(()),
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

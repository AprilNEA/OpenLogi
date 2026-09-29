//! Per-route setting intents, shared by foreground and background writers.
//!
//! A receiver lease is not a write queue. Hold a ticket's turn through readback;
//! retries reuse their policy ticket instead of becoming a new user intent.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use openlogi_hid::DeviceRoute;

/// Independent instances isolate settings; routes isolate keyboards.
#[derive(Clone, Default)]
pub(crate) struct WriteOrder<T>(Arc<Mutex<HashMap<String, Arc<Queue<T>>>>>);

#[derive(Default)]
struct Queue<T> {
    state: Mutex<State<T>>,
    turn: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State<T> {
    next: u64,
    committed: Option<Intent<T>>,
    policy: Option<Intent<T>>,
    pending: BTreeSet<u64>,
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
pub(crate) struct WriteTicket<T> {
    queue: Arc<Queue<T>>,
    number: u64,
}

impl<T: Clone + Default + Eq> WriteOrder<T> {
    fn queue(&self, route: &DeviceRoute) -> Arc<Queue<T>> {
        Arc::clone(
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(route.to_string())
                .or_default(),
        )
    }

    /// Immediately publish a new intent (the existing Fn-lock contract).
    pub(crate) fn request(&self, route: &DeviceRoute, value: T) -> WriteTicket<T> {
        let queue = self.queue(route);
        let number = {
            let mut state = queue.state();
            let intent = state.intent(value);
            let number = intent.number;
            state.committed = Some(intent);
            number
        };
        WriteTicket { queue, number }
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
    }

    /// Reuse the configured intent, including while a newer RPC supersedes it.
    pub(crate) fn policy(&self, route: &DeviceRoute) -> Option<(T, WriteTicket<T>)> {
        let queue = self.queue(route);
        let intent = queue.state().policy.clone()?;
        Some((
            intent.value,
            WriteTicket {
                queue,
                number: intent.number,
            },
        ))
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
            },
            intent,
        }
    }
}

impl<T> WriteTicket<T> {
    /// Earlier writes finish before a current ticket can start HID I/O.
    pub(crate) async fn turn(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let turn = self.queue.turn.lock().await;
        self.is_current().then_some(turn)
    }

    pub(crate) fn is_current(&self) -> bool {
        self.queue.state().latest() == self.number
    }
}

/// RAII prevents failed/cancelled RPCs from permanently retiring saved policy.
pub(crate) struct PendingWrite<T> {
    ticket: WriteTicket<T>,
    intent: Intent<T>,
}

impl<T: Clone> PendingWrite<T> {
    pub(crate) fn ticket(&self) -> WriteTicket<T> {
        self.ticket.clone()
    }

    /// A late success may record what finished, but cannot displace a newer
    /// committed intent. Return whether this request still owns the result.
    pub(crate) fn confirm(self) -> bool {
        let mut state = self.ticket.queue.state();
        if state
            .committed
            .as_ref()
            .is_none_or(|intent| intent.number < self.intent.number)
        {
            state.committed = Some(self.intent.clone());
        }
        state.latest() == self.intent.number
    }
}

impl<T> Drop for PendingWrite<T> {
    fn drop(&mut self) {
        self.ticket
            .queue
            .state()
            .pending
            .remove(&self.intent.number);
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
        let order = WriteOrder::default();
        let stale = order.request(&route(1), false);
        let newer = order.request(&route(1), true);
        assert!(stale.turn().await.is_none());
        assert!(newer.turn().await.is_some());
    }

    #[tokio::test]
    async fn a_write_requested_while_another_runs_waits_its_turn() {
        let order = WriteOrder::default();
        let running = order.request(&route(1), false);
        let turn = running.turn().await.expect("current");
        let next = order.request(&route(1), true);
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
        let first = order.request(&route(1), false);
        let _other = order.request(&route(2), true);
        let _other_setting = other_setting.request(&route(1), true);
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

//! Which device-wide haptic enables the agent still owes back.
//!
//! Arming may turn a device's haptic engine on for a session
//! ([`openlogi_hid::ensure_haptics_armed_on`]). That flag is device-wide, so
//! leaving it on would override a user who turned haptics off in another app.
//! This ledger decides when each enable is handed back; the worker in
//! [`super::RingHapticPlayer`] does the I/O.

use std::time::{Duration, Instant};

use openlogi_agent_core::action_ring::SESSION_LIFETIME;
use openlogi_hid::{DeviceRoute, HidppFeatureErrorKind, WriteError};

/// How long a restore that could not reach its device waits for its next
/// attempt.
///
/// A device that went away (asleep, switched to another host, out of range)
/// owes its restore until it answers again. An attempt with no channel to
/// the device sends nothing, and one through a receiver costs one short
/// exchange, so this pace stays cheap while the device is away and hands
/// the engine back soon after it returns.
pub(super) const RESTORE_RETRY: Duration = Duration::from_secs(10);

/// Who opened a haptic session.
///
/// Each lane holds at most one open session: a ring replaces the ring
/// before it, and a client burst the burst before it. The two lanes are
/// independent, so a ring and a burst can play on one engine at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lane {
    /// The Actions Ring.
    Ring,
    /// A client's burst of plays.
    Client,
}

/// One haptic session: its lane and its ID, which only grows within a lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Session {
    pub(super) lane: Lane,
    pub(super) id: u64,
}

/// One device whose haptic engine a session turned on.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Owed<R> {
    /// The device to hand the engine back on.
    pub(super) route: DeviceRoute,
    /// What arming proved, for the restore to check the device against.
    pub(super) restore: R,
    /// When the restore is attempted once no open session plays on the
    /// engine.
    due: Instant,
}

/// A lane's open session.
#[derive(Debug)]
struct Open {
    id: u64,
    route: DeviceRoute,
    /// When the session is over without a close.
    ends: Instant,
}

/// What one lane has opened and closed.
#[derive(Debug, Default)]
struct LaneState {
    /// The newest session the lane opened or closed. A session no newer is
    /// never opened again: its close already arrived, or it is open.
    newest: Option<u64>,
    open: Option<Open>,
}

/// The enables still owed back, at most one per device, and the sessions
/// still playing on them.
///
/// An engine is handed back only once no open session plays on it: a ring
/// closing does not turn off the engine a client burst still plays on, nor
/// the other way round.
#[derive(Debug)]
pub(super) struct RestoreLedger<R> {
    owed: Vec<Owed<R>>,
    ring: LaneState,
    client: LaneState,
}

impl<R> Default for RestoreLedger<R> {
    fn default() -> Self {
        Self {
            owed: Vec::new(),
            ring: LaneState::default(),
            client: LaneState::default(),
        }
    }
}

impl<R> RestoreLedger<R> {
    /// Whether `session` is newer than anything its lane has opened or
    /// closed, so arming it is still owed.
    pub(super) fn is_new(&self, session: Session) -> bool {
        self.lane(session.lane)
            .newest
            .is_none_or(|newest| session.id > newest)
    }

    /// `session` opened on `route`, and arming it returned `restore`: `Some`
    /// when that arming turned the engine on. A session that is not
    /// [new](Self::is_new) is ignored.
    ///
    /// The session replaces its lane's previous one, which is over. An
    /// enable `route` already owes stays owed until this session is over
    /// too: the session plays on the engine that enable left on, and
    /// handing it back first would only turn it off and on again. A session
    /// that never reports its close is over [`SESSION_LIFETIME`] after `now`.
    /// Every engine no session plays on any more is handed back at `now`.
    pub(super) fn opened(
        &mut self,
        session: Session,
        route: DeviceRoute,
        restore: Option<R>,
        now: Instant,
    ) {
        if !self.is_new(session) {
            return;
        }
        let lane = self.lane_mut(session.lane);
        lane.newest = Some(session.id);
        lane.open = Some(Open {
            id: session.id,
            route: route.clone(),
            ends: now + SESSION_LIFETIME,
        });
        self.due_at_latest(now);
        if let Some(restore) = restore {
            if let Some(owed) = self.owed.iter_mut().find(|owed| owed.route == route) {
                owed.restore = restore;
            } else {
                self.owed.push(Owed {
                    route,
                    restore,
                    due: now,
                });
            }
        }
    }

    /// `session` closed, and with it any older session of its lane. Restores
    /// no other open session still plays on are due at `now`. A close older
    /// than its lane's open session leaves that session open: a close that
    /// arrives late must not turn off the engine the next session plays on.
    pub(super) fn closed(&mut self, session: Session, now: Instant) {
        let lane = self.lane_mut(session.lane);
        lane.newest = lane.newest.max(Some(session.id));
        if lane.open.as_ref().is_some_and(|open| open.id <= session.id) {
            lane.open = None;
        }
        self.due_at_latest(now);
    }

    /// The agent is leaving, or handing over to a replacement, which ends
    /// every session: every restore is due at `now`.
    pub(super) fn leaving(&mut self, now: Instant) {
        self.ring.open = None;
        self.client.open = None;
        self.due_at_latest(now);
    }

    /// When the next restore is due, or `None` when nothing is owed.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.owed.iter().map(|owed| self.due(owed)).min()
    }

    /// Take one restore that is due at `now`. It is no longer owed unless
    /// the caller hands it back through [`Self::retry`].
    pub(super) fn take_due(&mut self, now: Instant) -> Option<Owed<R>> {
        for lane in [&mut self.ring, &mut self.client] {
            if lane.open.as_ref().is_some_and(|open| open.ends <= now) {
                lane.open = None;
            }
        }
        let index = self.owed.iter().position(|owed| self.due(owed) <= now)?;
        Some(self.owed.swap_remove(index))
    }

    /// The restore taken as `owed` could not reach its device: owe it again,
    /// due at `now + RESTORE_RETRY` unless a close or another session makes it
    /// due sooner.
    ///
    /// The worker calls this right after the attempt, with no session opened
    /// in between, so the device owes nothing else. Should a session ever
    /// open in between, its own entry is the newer proof and this one is
    /// dropped.
    pub(super) fn retry(&mut self, mut owed: Owed<R>, now: Instant) {
        if self.owed.iter().any(|newer| newer.route == owed.route) {
            return;
        }
        owed.due = now + RESTORE_RETRY;
        self.owed.push(owed);
    }

    /// When `owed` is due: once the last open session on its device is
    /// over, or at its own time when none is.
    fn due(&self, owed: &Owed<R>) -> Instant {
        [&self.ring, &self.client]
            .into_iter()
            .filter_map(|lane| lane.open.as_ref())
            .filter(|open| open.route == owed.route)
            .map(|open| open.ends)
            .max()
            .unwrap_or(owed.due)
    }

    /// A session opening or closing is a fresh chance to reach every device:
    /// each restore is due at `now` at the latest, once no open session plays
    /// on its engine (see [`Self::due`]).
    fn due_at_latest(&mut self, now: Instant) {
        for owed in &mut self.owed {
            owed.due = owed.due.min(now);
        }
    }

    fn lane(&self, lane: Lane) -> &LaneState {
        match lane {
            Lane::Ring => &self.ring,
            Lane::Client => &self.client,
        }
    }

    fn lane_mut(&mut self, lane: Lane) -> &mut LaneState {
        match lane {
            Lane::Ring => &mut self.ring,
            Lane::Client => &mut self.client,
        }
    }
}

/// Whether a failed restore never reached the device, so a later attempt can
/// still hand the engine back.
///
/// A missing channel, a device that does not answer, a lost or late reply,
/// and a busy device all clear up on their own. Every other failure is the
/// device's answer — no haptic feature at that route any more, or a reply
/// that refuses the restore — and asking again would get the same answer.
pub(super) fn is_unreached(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::DeviceNotFound
            | WriteError::DeviceUnreachable { .. }
            | WriteError::RequestTimedOut { .. }
            | WriteError::Hid(_)
            | WriteError::Hidpp(_)
            | WriteError::HidppFeature {
                kind: HidppFeatureErrorKind::Busy,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use openlogi_hid::HidppOperation;

    use super::*;

    fn mouse() -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb042,
        }
    }

    fn other_mouse() -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb034,
        }
    }

    fn ring(id: u64) -> Session {
        Session {
            lane: Lane::Ring,
            id,
        }
    }

    fn client(id: u64) -> Session {
        Session {
            lane: Lane::Client,
            id,
        }
    }

    /// Every restore due at `now`, as (route, proof) pairs.
    fn due(ledger: &mut RestoreLedger<u8>, now: Instant) -> Vec<(DeviceRoute, u8)> {
        std::iter::from_fn(|| ledger.take_due(now))
            .map(|owed| (owed.route, owed.restore))
            .collect()
    }

    #[test]
    fn a_session_that_turned_the_engine_on_hands_it_back_when_it_closes() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        let closed = start + Duration::from_secs(2);
        assert_eq!(due(&mut ledger, closed), vec![], "still open");
        ledger.closed(ring(1), closed);
        assert_eq!(due(&mut ledger, closed), vec![(mouse(), 7)]);
        assert_eq!(ledger.next_due(), None, "handed back once");
    }

    #[test]
    fn a_session_that_found_the_engine_on_owes_nothing() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::<u8>::default();
        ledger.opened(ring(1), mouse(), None, start);
        assert_eq!(ledger.next_due(), None);
        ledger.closed(ring(1), start);
        assert_eq!(due(&mut ledger, start + SESSION_LIFETIME), vec![]);
    }

    #[test]
    fn a_session_whose_close_never_arrives_is_handed_back_once_it_must_have_expired() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        assert_eq!(ledger.next_due(), Some(start + SESSION_LIFETIME));
        let just_before = start + SESSION_LIFETIME.saturating_sub(Duration::from_millis(1));
        assert_eq!(due(&mut ledger, just_before), vec![]);
        assert_eq!(
            due(&mut ledger, start + SESSION_LIFETIME),
            vec![(mouse(), 7)]
        );
    }

    #[test]
    fn a_late_close_does_not_turn_off_the_engine_a_newer_session_plays_on() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        // The next session on the same mouse finds the engine on: the enable
        // it plays on is still session 1's, so it carries the restore.
        let reopened = start + Duration::from_secs(1);
        ledger.opened(ring(2), mouse(), None, reopened);
        ledger.closed(ring(1), reopened);
        assert_eq!(due(&mut ledger, reopened), vec![]);
        assert_eq!(ledger.next_due(), Some(reopened + SESSION_LIFETIME));
        ledger.closed(ring(2), reopened);
        assert_eq!(due(&mut ledger, reopened), vec![(mouse(), 7)]);
    }

    #[test]
    fn a_newer_arming_replaces_the_proof_it_supersedes() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        // Something cleared the flag between sessions; arming set it again.
        ledger.opened(ring(2), mouse(), Some(8), start);
        ledger.closed(ring(2), start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 8)]);
    }

    #[test]
    fn a_session_on_another_device_ends_the_previous_one() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        let switched = start + Duration::from_secs(1);
        ledger.opened(ring(2), other_mouse(), Some(9), switched);
        assert_eq!(due(&mut ledger, switched), vec![(mouse(), 7)]);
        ledger.closed(ring(2), switched);
        assert_eq!(due(&mut ledger, switched), vec![(other_mouse(), 9)]);
    }

    #[test]
    fn leaving_hands_back_an_open_session_and_a_pending_retry_at_once() {
        let start = Instant::now();
        // A session still open.
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        assert_eq!(due(&mut ledger, start), vec![]);
        ledger.leaving(start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 7)]);
        // A restore waiting for its next attempt.
        ledger.opened(ring(2), mouse(), Some(8), start);
        ledger.closed(ring(2), start);
        let owed = ledger.take_due(start).expect("due at the close");
        ledger.retry(owed, start);
        assert_eq!(due(&mut ledger, start), vec![]);
        ledger.leaving(start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 8)]);
    }

    #[test]
    fn an_unreached_device_is_tried_again_later() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.closed(ring(1), start);
        let owed = ledger.take_due(start).expect("due at the close");
        ledger.retry(owed, start);
        assert_eq!(ledger.next_due(), Some(start + RESTORE_RETRY));
        assert_eq!(
            due(
                &mut ledger,
                start + RESTORE_RETRY.saturating_sub(Duration::from_millis(1))
            ),
            vec![]
        );
        // A ring opened on another device is a fresh chance to try, so the
        // restore does not wait for the pace.
        let switched = start + Duration::from_secs(1);
        ledger.opened(ring(2), other_mouse(), None, switched);
        assert_eq!(due(&mut ledger, switched), vec![(mouse(), 7)]);
    }

    #[test]
    fn an_unreached_device_that_opens_a_session_waits_for_that_session() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.closed(ring(1), start);
        let owed = ledger.take_due(start).expect("due at the close");
        ledger.retry(owed, start);
        // The mouse is back and opens a ring: the engine stays on for it.
        let back = start + Duration::from_secs(30);
        ledger.opened(ring(2), mouse(), None, back);
        assert_eq!(due(&mut ledger, back), vec![]);
        ledger.closed(ring(2), back);
        assert_eq!(due(&mut ledger, back), vec![(mouse(), 7)]);
    }

    #[test]
    fn a_retry_never_duplicates_a_device_that_owes_a_newer_restore() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.closed(ring(1), start);
        let stale = ledger.take_due(start).expect("due at the close");
        ledger.opened(ring(2), mouse(), Some(8), start);
        ledger.retry(stale, start);
        ledger.closed(ring(2), start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 8)]);
        assert_eq!(ledger.next_due(), None);
    }

    #[test]
    fn a_ring_and_a_burst_on_one_engine_hand_it_back_once_both_closed() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        // Ring first, the burst closing first; then the burst first, the
        // ring closing first.
        for (first, second) in [(ring(1), client(2)), (client(1), ring(2))] {
            for closing_first in [first, second] {
                let closing_last = if closing_first == first {
                    second
                } else {
                    first
                };
                let mut ledger = RestoreLedger::default();
                ledger.opened(first, mouse(), Some(7), start);
                // The second finds the engine the first turned on.
                ledger.opened(second, mouse(), None, start);
                ledger.closed(closing_first, later);
                assert_eq!(
                    due(&mut ledger, later),
                    vec![],
                    "{closing_last:?} still plays on the engine"
                );
                assert_eq!(ledger.next_due(), Some(start + SESSION_LIFETIME));
                ledger.closed(closing_last, later);
                assert_eq!(due(&mut ledger, later), vec![(mouse(), 7)]);
                assert_eq!(ledger.next_due(), None, "handed back once");
            }
        }
    }

    #[test]
    fn a_close_in_one_lane_never_closes_the_other_lanes_older_session() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.opened(client(2), other_mouse(), Some(9), start);
        // A newer close, but the burst's: the ring on the other mouse
        // stays open.
        ledger.closed(client(2), start);
        assert_eq!(due(&mut ledger, start), vec![(other_mouse(), 9)]);
        assert_eq!(ledger.next_due(), Some(start + SESSION_LIFETIME));
        ledger.closed(ring(1), start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 7)]);
    }

    #[test]
    fn a_session_whose_close_arrived_first_or_that_is_open_is_never_new() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::<u8>::default();
        assert!(ledger.is_new(ring(3)));
        ledger.closed(ring(3), start);
        assert!(!ledger.is_new(ring(3)), "closed before it was armed");
        assert!(
            ledger.is_new(client(3)),
            "the other lane keeps its own count"
        );
        ledger.opened(client(3), mouse(), None, start);
        assert!(!ledger.is_new(client(3)), "already armed");
        assert!(!ledger.is_new(client(2)), "older than the open burst");
        assert!(ledger.is_new(client(4)));
        // An arming for a session that is not new changes nothing.
        ledger.opened(client(2), other_mouse(), Some(9), start);
        assert_eq!(ledger.next_due(), None);
    }

    #[test]
    fn a_burst_that_moves_to_another_device_hands_back_the_one_it_left() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.opened(client(2), mouse(), None, start);
        ledger.opened(client(3), other_mouse(), Some(9), start);
        assert_eq!(
            due(&mut ledger, start),
            vec![],
            "the ring still plays on the mouse"
        );
        ledger.opened(client(4), mouse(), None, start);
        assert_eq!(due(&mut ledger, start), vec![(other_mouse(), 9)]);
    }

    #[test]
    fn leaving_ends_both_lanes() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(ring(1), mouse(), Some(7), start);
        ledger.opened(client(2), other_mouse(), Some(9), start);
        ledger.leaving(start);
        let mut handed_back = due(&mut ledger, start);
        handed_back.sort_by_key(|(_, proof)| *proof);
        assert_eq!(handed_back, vec![(mouse(), 7), (other_mouse(), 9)]);
    }

    #[test]
    fn only_failures_that_never_reached_the_device_are_tried_again() {
        let operation = HidppOperation::PlayHaptic;
        let feature_hex = 0x19b0;
        for unreached in [
            WriteError::DeviceNotFound,
            WriteError::DeviceUnreachable { index: 1 },
            WriteError::RequestTimedOut { operation },
            WriteError::Hid("closed".to_owned()),
            WriteError::Hidpp("channel".to_owned()),
            WriteError::HidppFeature {
                operation,
                feature_hex,
                kind: HidppFeatureErrorKind::Busy,
            },
        ] {
            assert!(is_unreached(&unreached), "{unreached:?} is retried");
        }
        for answered in [
            WriteError::FeatureUnsupported { feature_hex },
            WriteError::UnsupportedResponse {
                operation,
                feature_hex,
            },
            WriteError::HidppFeature {
                operation,
                feature_hex,
                kind: HidppFeatureErrorKind::InvalidArgument,
            },
        ] {
            assert!(
                !is_unreached(&answered),
                "{answered:?} is the device's answer"
            );
        }
    }
}

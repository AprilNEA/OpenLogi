//! Which device-wide haptic enables the Actions Ring still owes back.
//!
//! Arming may turn a device's haptic engine on for a ring session
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

/// One device whose haptic engine a ring session turned on.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Owed<R> {
    /// The device to hand the engine back on.
    pub(super) route: DeviceRoute,
    /// What arming proved, for the restore to check the device against.
    pub(super) restore: R,
    /// The newest session that may still play on the engine.
    session: u64,
    /// When the restore is attempted without waiting any longer for a close.
    due: Instant,
}

/// The enables still owed back, at most one per device.
#[derive(Debug)]
pub(super) struct RestoreLedger<R> {
    owed: Vec<Owed<R>>,
}

impl<R> Default for RestoreLedger<R> {
    fn default() -> Self {
        Self { owed: Vec::new() }
    }
}

impl<R> RestoreLedger<R> {
    /// Session `session` opened on `route`, and arming it returned `restore`:
    /// `Some` when that arming turned the engine on.
    ///
    /// Only one session is open at a time, so every other device's session
    /// has ended, and its restore is due at `now`. This device's restore, if
    /// it already owes one, moves to the new session instead: the session
    /// plays on the engine that enable left on, and handing it back first
    /// would only turn it off and on again. A session that never reports its
    /// close is over [`SESSION_LIFETIME`] after `now`.
    pub(super) fn opened(
        &mut self,
        session: u64,
        route: DeviceRoute,
        restore: Option<R>,
        now: Instant,
    ) {
        let ends = now + SESSION_LIFETIME;
        for owed in &mut self.owed {
            if owed.route != route {
                owed.due = owed.due.min(now);
            }
        }
        if let Some(owed) = self.owed.iter_mut().find(|owed| owed.route == route) {
            owed.session = session;
            owed.due = ends;
            if let Some(restore) = restore {
                owed.restore = restore;
            }
        } else if let Some(restore) = restore {
            self.owed.push(Owed {
                route,
                restore,
                session,
                due: ends,
            });
        }
    }

    /// Session `session` closed. Restores owed for it, or for an older
    /// session, are due at `now`. A newer session's restore waits: a close
    /// that arrives late must not turn off the engine the next session plays
    /// on.
    pub(super) fn closed(&mut self, session: u64, now: Instant) {
        for owed in &mut self.owed {
            if owed.session <= session {
                owed.due = owed.due.min(now);
            }
        }
    }

    /// The agent is leaving, or handing over to a replacement, which ends
    /// every session: every restore is due at `now`.
    pub(super) fn leaving(&mut self, now: Instant) {
        for owed in &mut self.owed {
            owed.due = owed.due.min(now);
        }
    }

    /// When the next restore is due, or `None` when nothing is owed.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.owed.iter().map(|owed| owed.due).min()
    }

    /// Take one restore that is due at `now`. It is no longer owed unless
    /// the caller hands it back through [`Self::retry`].
    pub(super) fn take_due(&mut self, now: Instant) -> Option<Owed<R>> {
        let index = self.owed.iter().position(|owed| owed.due <= now)?;
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
        ledger.opened(1, mouse(), Some(7), start);
        let closed = start + Duration::from_secs(2);
        assert_eq!(due(&mut ledger, closed), vec![], "still open");
        ledger.closed(1, closed);
        assert_eq!(due(&mut ledger, closed), vec![(mouse(), 7)]);
        assert_eq!(ledger.next_due(), None, "handed back once");
    }

    #[test]
    fn a_session_that_found_the_engine_on_owes_nothing() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::<u8>::default();
        ledger.opened(1, mouse(), None, start);
        assert_eq!(ledger.next_due(), None);
        ledger.closed(1, start);
        assert_eq!(due(&mut ledger, start + SESSION_LIFETIME), vec![]);
    }

    #[test]
    fn a_session_whose_close_never_arrives_is_handed_back_once_it_must_have_expired() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
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
        ledger.opened(1, mouse(), Some(7), start);
        // The next session on the same mouse finds the engine on: the enable
        // it plays on is still session 1's, so it carries the restore.
        let reopened = start + Duration::from_secs(1);
        ledger.opened(2, mouse(), None, reopened);
        ledger.closed(1, reopened);
        assert_eq!(due(&mut ledger, reopened), vec![]);
        assert_eq!(ledger.next_due(), Some(reopened + SESSION_LIFETIME));
        ledger.closed(2, reopened);
        assert_eq!(due(&mut ledger, reopened), vec![(mouse(), 7)]);
    }

    #[test]
    fn a_newer_arming_replaces_the_proof_it_supersedes() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
        // Something cleared the flag between sessions; arming set it again.
        ledger.opened(2, mouse(), Some(8), start);
        ledger.closed(2, start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 8)]);
    }

    #[test]
    fn a_session_on_another_device_ends_the_previous_one() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
        let switched = start + Duration::from_secs(1);
        ledger.opened(2, other_mouse(), Some(9), switched);
        assert_eq!(due(&mut ledger, switched), vec![(mouse(), 7)]);
        ledger.closed(2, switched);
        assert_eq!(due(&mut ledger, switched), vec![(other_mouse(), 9)]);
    }

    #[test]
    fn leaving_hands_back_an_open_session_and_a_pending_retry_at_once() {
        let start = Instant::now();
        // A session still open.
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
        assert_eq!(due(&mut ledger, start), vec![]);
        ledger.leaving(start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 7)]);
        // A restore waiting for its next attempt.
        ledger.opened(2, mouse(), Some(8), start);
        ledger.closed(2, start);
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
        ledger.opened(1, mouse(), Some(7), start);
        ledger.closed(1, start);
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
        ledger.opened(2, other_mouse(), None, switched);
        assert_eq!(due(&mut ledger, switched), vec![(mouse(), 7)]);
    }

    #[test]
    fn an_unreached_device_that_opens_a_session_waits_for_that_session() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
        ledger.closed(1, start);
        let owed = ledger.take_due(start).expect("due at the close");
        ledger.retry(owed, start);
        // The mouse is back and opens a ring: the engine stays on for it.
        let back = start + Duration::from_secs(30);
        ledger.opened(2, mouse(), None, back);
        assert_eq!(due(&mut ledger, back), vec![]);
        ledger.closed(2, back);
        assert_eq!(due(&mut ledger, back), vec![(mouse(), 7)]);
    }

    #[test]
    fn a_retry_never_duplicates_a_device_that_owes_a_newer_restore() {
        let start = Instant::now();
        let mut ledger = RestoreLedger::default();
        ledger.opened(1, mouse(), Some(7), start);
        ledger.closed(1, start);
        let stale = ledger.take_due(start).expect("due at the close");
        ledger.opened(2, mouse(), Some(8), start);
        ledger.retry(stale, start);
        ledger.closed(2, start);
        assert_eq!(due(&mut ledger, start), vec![(mouse(), 8)]);
        assert_eq!(ledger.next_due(), None);
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

//! One keyboard's lock-key state on an inventory-owned channel, kept to tell
//! which key a `0x4220` broadcast changed.
//!
//! The state comes from two places: the read an inventory scan starts once
//! per connection, and the keyboard's own broadcasts. [`LockKeyState`] owns
//! how they combine: when a read is due, which answer still counts, and what a
//! broadcast is compared with.

use hidpp::feature::lock_key_state::LockKeys;

/// One device's lock-key state, and which of its connections it is about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LockKeyState {
    /// How often the device came back since it was registered. A read is
    /// stamped with it, so a read started before a comeback does not record
    /// its answer after it.
    connection: u64,
    phase: Phase,
}

/// What this connection knows of the lock keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// Nothing yet: the next scan reads the state.
    #[default]
    Unread,
    /// The connection's one read is in flight, or was dropped with a scan
    /// that timed out. Either way it is not started again.
    Reading,
    /// The state, from the read's answer or from a broadcast.
    Known(LockKeys),
    /// The connection's one read failed. It is not retried on every scan;
    /// the next broadcast records the state instead.
    GaveUp,
}

/// The connection a lock-state read started on. Only
/// [`LockKeyState::start_read`] makes one, so only a started read can
/// record its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReadConnection(u64);

impl LockKeyState {
    /// Forget the state: the device was away, and its keys may have changed
    /// meanwhile, so the next scan reads them again. A read still in flight
    /// from before records nothing.
    pub(super) fn forget(&mut self) {
        self.connection = self.connection.wrapping_add(1);
        self.phase = Phase::Unread;
    }

    /// Start this connection's one read, unless one was started already or
    /// a broadcast told the state first. Returns the connection to record
    /// the read's result against.
    pub(super) fn start_read(&mut self) -> Option<ReadConnection> {
        if self.phase != Phase::Unread {
            return None;
        }
        self.phase = Phase::Reading;
        Some(ReadConnection(self.connection))
    }

    /// Record the state the read started on `connection` answered, unless a
    /// broadcast recorded a state while the read was in flight, or the
    /// device came back meanwhile: then the answer is from the connection
    /// before.
    pub(super) fn record_read_answer(&mut self, connection: ReadConnection, keys: LockKeys) {
        if self.is_reading(connection) {
            self.phase = Phase::Known(keys);
        }
    }

    /// Give up the read started on `connection`, under the same conditions
    /// as [`Self::record_read_answer`].
    pub(super) fn record_read_failure(&mut self, connection: ReadConnection) {
        if self.is_reading(connection) {
            self.phase = Phase::GaveUp;
        }
    }

    /// Record the state a broadcast carries, and return the state it
    /// replaces, to compare with. `None` when no state was known: then the
    /// broadcast only records its state, because no key can be named as
    /// changed.
    pub(super) fn record_broadcast(&mut self, keys: LockKeys) -> Option<LockKeys> {
        let previous = match self.phase {
            Phase::Known(previous) => Some(previous),
            Phase::Unread | Phase::Reading | Phase::GaveUp => None,
        };
        self.phase = Phase::Known(keys);
        previous
    }

    fn is_reading(&self, connection: ReadConnection) -> bool {
        self.phase == Phase::Reading && connection == ReadConnection(self.connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: LockKeys = LockKeys::CAPS_LOCK;
    const NUM: LockKeys = LockKeys::NUM_LOCK;

    /// Whether a scan would read `state` now. Probed on a copy.
    fn reads(mut state: LockKeyState) -> bool {
        state.start_read().is_some()
    }

    /// The state a broadcast to `state` would be compared with. Probed on a
    /// copy.
    fn compared_with(mut state: LockKeyState) -> Option<LockKeys> {
        state.record_broadcast(LockKeys::empty())
    }

    #[test]
    fn every_phase_reads_compares_and_forgets() {
        let unread = LockKeyState::default();
        let mut reading = unread;
        let read = reading.start_read().expect("a first read");
        let mut read_answered = reading;
        read_answered.record_read_answer(read, NUM);
        let mut broadcast_first = unread;
        assert_eq!(broadcast_first.record_broadcast(NUM), None);
        let mut gave_up = reading;
        gave_up.record_read_failure(read);

        // (phase, a scan reads it, what a broadcast is compared with)
        let table = [
            ("unread", unread, true, None),
            ("reading", reading, false, None),
            ("known by read", read_answered, false, Some(NUM)),
            // A broadcast tells the whole state, so no read is due.
            ("known by broadcast", broadcast_first, false, Some(NUM)),
            ("gave up", gave_up, false, None),
        ];
        for (phase, state, read_due, compared) in table {
            assert_eq!(reads(state), read_due, "{phase}: read");
            assert_eq!(compared_with(state), compared, "{phase}: compared");

            let mut broadcast = state;
            assert_eq!(broadcast.record_broadcast(CAPS), compared, "{phase}");
            assert_eq!(
                (reads(broadcast), compared_with(broadcast)),
                (false, Some(CAPS)),
                "{phase}: after a broadcast"
            );

            let mut forgotten = state;
            forgotten.forget();
            assert_eq!(
                (reads(forgotten), compared_with(forgotten)),
                (true, None),
                "{phase}: forgotten"
            );
        }
    }

    #[test]
    fn a_broadcast_during_a_read_wins_over_its_result() {
        let mut state = LockKeyState::default();
        let read = state.start_read().expect("a first read");
        assert_eq!(state.record_broadcast(CAPS), None);

        let mut answered = state;
        answered.record_read_answer(read, NUM);
        assert_eq!(compared_with(answered), Some(CAPS));

        let mut failed = state;
        failed.record_read_failure(read);
        assert_eq!(compared_with(failed), Some(CAPS));
    }

    #[test]
    fn an_answer_from_before_a_comeback_is_dropped() {
        let mut state = LockKeyState::default();
        let old = state.start_read().expect("a first read");
        state.forget();

        // Before the new connection's read starts.
        let mut early = state;
        early.record_read_answer(old, CAPS);
        assert_eq!(compared_with(early), None);
        assert!(reads(early), "the new connection still reads");

        // While the new connection's read is in flight.
        let new = state.start_read().expect("read again");
        state.record_read_answer(old, CAPS);
        assert_eq!(compared_with(state), None);
        state.record_read_answer(new, NUM);
        assert_eq!(compared_with(state), Some(NUM));
    }

    #[test]
    fn a_failure_from_before_a_comeback_does_not_give_up_the_new_read() {
        let mut state = LockKeyState::default();
        let old = state.start_read().expect("a first read");
        state.forget();
        let new = state.start_read().expect("read again");

        state.record_read_failure(old);
        state.record_read_answer(new, CAPS);
        assert_eq!(compared_with(state), Some(CAPS));
    }
}

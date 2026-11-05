//! Holds back the wheel's spring-back as a scroll stops.
//!
//! When a finger stops the main wheel, the wheel can spring back by a step
//! right after its last forward tick, and that bounce scrolls the page
//! backwards. Only that moment is guarded: for a short lock period after the
//! last tick in one direction, the first step the other way — up to
//! [`BOUNCE_MAX_NOTCHES`], however many reports it arrives in — is held
//! instead of scrolled. Anything more is a real reversal: the held motion and
//! the new tick are released together, so nothing is lost and the page turns
//! around at once. A held step is dropped as a bounce when the wheel turns
//! forward again, or when the next tick comes after the lock has run out.
//!
//! Everywhere else — mid-scroll, or scrolling back and forth deliberately —
//! ticks pass straight through, and the caller turns the lock off where a
//! wheel cannot spring back.

use std::time::{Duration, Instant};

/// The most opposite-direction motion a spring-back is: one step. Any more
/// within the lock is a deliberate reversal.
const BOUNCE_MAX_NOTCHES: f64 = 1.0;

/// What to do with one tick.
#[derive(Debug, PartialEq)]
pub enum Admit<T> {
    /// Scroll it now.
    Pass(T),
    /// Hold it: it may be a bounce.
    Hold,
    /// A reversal is confirmed: scroll every held tick and this one, oldest
    /// first.
    Release(Vec<T>),
}

/// One wheel's lock state.
pub struct ReverseLock<T> {
    /// Direction (±1) and instant of the last tick that scrolled.
    last: Option<(f64, Instant)>,
    held: Vec<T>,
    held_notches: f64,
}

impl<T> Default for ReverseLock<T> {
    fn default() -> Self {
        Self {
            last: None,
            held: Vec::new(),
            held_notches: 0.0,
        }
    }
}

impl<T> ReverseLock<T> {
    /// Decide on a tick of signed `notches` at `at`, carrying `payload`.
    /// `lock` is how long after the last scrolled tick a step the other way
    /// is held; zero turns the lock off.
    pub fn admit(&mut self, notches: f64, at: Instant, lock: Duration, payload: T) -> Admit<T> {
        let direction = notches.signum();
        let Some((last_direction, last_at)) = self.last else {
            self.last = Some((direction, at));
            return Admit::Pass(payload);
        };
        let locked = !lock.is_zero() && at.saturating_duration_since(last_at) < lock;
        if direction * last_direction > 0.0 || !locked {
            // Forward again, or the lock ran out: whatever was held was a
            // bounce.
            self.drop_held();
            self.last = Some((direction, at));
            return Admit::Pass(payload);
        }
        self.held.push(payload);
        self.held_notches += notches.abs();
        if self.held_notches > BOUNCE_MAX_NOTCHES {
            // More than one step: a real reversal.
            self.held_notches = 0.0;
            self.last = Some((direction, at));
            return Admit::Release(std::mem::take(&mut self.held));
        }
        Admit::Hold
    }

    fn drop_held(&mut self) {
        self.held.clear();
        self.held_notches = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: Duration = Duration::from_millis(100);

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_one_step_spring_back_as_the_wheel_stops_is_dropped() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        for t in [0, 8, 15, 23] {
            assert_eq!(lock.admit(-1.0, ms(start, t), LOCK, t), Admit::Pass(t));
        }
        // One step back 60 ms after the last forward tick.
        assert_eq!(lock.admit(1.0, ms(start, 83), LOCK, 83), Admit::Hold);
        // The next scroll the original way drops it.
        assert_eq!(
            lock.admit(-1.0, ms(start, 900), LOCK, 900),
            Admit::Pass(900)
        );
        assert!(lock.held.is_empty());
    }

    #[test]
    fn a_high_resolution_spring_back_in_several_reports_is_one_step() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        assert_eq!(lock.admit(-0.5, ms(start, 0), LOCK, 0), Admit::Pass(0));
        for t in [40, 48, 56] {
            assert_eq!(lock.admit(0.3, ms(start, t), LOCK, t), Admit::Hold);
        }
        // A tick after the lock has run out: the held step was a bounce.
        assert_eq!(lock.admit(0.3, ms(start, 400), LOCK, 400), Admit::Pass(400));
        assert!(lock.held.is_empty());
    }

    #[test]
    fn a_second_step_back_is_a_reversal_released_without_loss() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        assert_eq!(lock.admit(1.0, ms(start, 0), LOCK, 0), Admit::Pass(0));
        assert_eq!(lock.admit(-1.0, ms(start, 40), LOCK, 1), Admit::Hold);
        assert_eq!(
            lock.admit(-1.0, ms(start, 60), LOCK, 2),
            Admit::Release(vec![1, 2])
        );
        // From now on that is the scrolling direction.
        assert_eq!(lock.admit(-1.0, ms(start, 70), LOCK, 3), Admit::Pass(3));
        assert!(lock.held.is_empty());
    }

    #[test]
    fn more_than_a_step_at_once_scrolls_back_at_once() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        assert_eq!(lock.admit(1.0, ms(start, 0), LOCK, 0), Admit::Pass(0));
        assert_eq!(
            lock.admit(-2.0, ms(start, 30), LOCK, 1),
            Admit::Release(vec![1])
        );
    }

    #[test]
    fn a_reversal_after_the_lock_passes_at_once() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        assert_eq!(lock.admit(1.0, ms(start, 0), LOCK, 0), Admit::Pass(0));
        assert_eq!(lock.admit(-1.0, ms(start, 150), LOCK, 1), Admit::Pass(1));
    }

    #[test]
    fn a_zero_lock_passes_everything() {
        let start = Instant::now();
        let mut lock = ReverseLock::default();
        assert_eq!(
            lock.admit(1.0, ms(start, 0), Duration::ZERO, 0),
            Admit::Pass(0)
        );
        assert_eq!(
            lock.admit(-1.0, ms(start, 5), Duration::ZERO, 1),
            Admit::Pass(1)
        );
    }
}

//! The exit velocity of a drag, measured from its samples' arrival times.
//!
//! Platform-neutral so it is tested everywhere; only macOS plays drags.

use std::time::{Duration, Instant};

/// The velocity of the latest timed step of a drag, per second.
///
/// Each sample carries the instant its report arrived, not the instant it was
/// handled: a batch of reports queued behind a busy thread keeps the pauses
/// between them. The first sample has no step before it, so the velocity
/// stays unmeasured until a second one arrives. A clock that steps backwards
/// reads as no time passing, which measures nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) enum FlickVelocity {
    /// No sample yet.
    #[default]
    Unsampled,
    /// One sample: no step to measure.
    Sampled { last_at: Instant },
    /// Two or more samples: the latest timed step's velocity.
    Measured {
        last_at: Instant,
        velocity: (f64, f64),
    },
}

impl FlickVelocity {
    /// Record a step of `dx`, `dy` whose report arrived `at`.
    pub(crate) fn sample(&mut self, dx: f64, dy: f64, at: Instant) {
        *self = match *self {
            Self::Unsampled => Self::Sampled { last_at: at },
            Self::Sampled { last_at } | Self::Measured { last_at, .. } => {
                let elapsed = at.saturating_duration_since(last_at).as_secs_f64();
                let last_at = last_at.max(at);
                match (*self, elapsed > 0.0) {
                    (_, true) => Self::Measured {
                        last_at,
                        velocity: (dx / elapsed, dy / elapsed),
                    },
                    (Self::Measured { velocity, .. }, false) => {
                        Self::Measured { last_at, velocity }
                    }
                    (_, false) => Self::Sampled { last_at },
                }
            }
        };
    }

    /// The velocity to glide on after a release `released_at`: `None` while
    /// unmeasured, or when the hand rested longer than `max_rest` before it.
    pub(crate) fn exit(&self, released_at: Instant, max_rest: Duration) -> Option<(f64, f64)> {
        match *self {
            Self::Measured { last_at, velocity }
                if released_at.saturating_duration_since(last_at) <= max_rest =>
            {
                Some(velocity)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REST: Duration = Duration::from_millis(50);

    #[test]
    fn drag_momentum_sample_timing() {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);

        // One sample is no step: nothing to glide on, however soon the release.
        let mut one = FlickVelocity::default();
        one.sample(80.0, 0.0, ms(0));
        assert_eq!(one.exit(ms(1), REST), None);

        // A delayed batch keeps the pause its reports arrived with: 8 points
        // over the 8 ms between arrivals, though both were handled together.
        let mut batch = FlickVelocity::default();
        batch.sample(8.0, 0.0, ms(100));
        batch.sample(8.0, 0.0, ms(108));
        let (vx, vy) = batch.exit(ms(110), REST).expect("measured");
        assert!((vx - 1000.0).abs() < 1e-6, "{vx}");
        assert!(vy.abs() < 1e-9);

        // A hand that rested before letting go launches nothing.
        assert_eq!(
            batch.exit(ms(108) + REST + Duration::from_millis(1), REST),
            None
        );
    }

    #[test]
    fn a_backwards_clock_step_measures_nothing() {
        let start = Instant::now() + Duration::from_secs(1);
        let mut drag = FlickVelocity::default();
        drag.sample(5.0, 0.0, start);
        drag.sample(
            5.0,
            0.0,
            start
                .checked_sub(Duration::from_millis(4))
                .expect("a second of headroom"),
        );
        assert_eq!(drag.exit(start, REST), None);
    }
}

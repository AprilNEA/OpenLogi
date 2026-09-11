//! Fractional magnification → whole Ctrl+wheel notches.
//!
//! Windows and Linux have no continuous magnification event; their only page
//! zoom idiom is Ctrl+wheel, whose smallest step is one notch. A wheel tick
//! carries less magnification than that, so the fraction has to accumulate
//! until it is worth a notch — otherwise a high-resolution wheel rounds every
//! step to zero and never zooms at all.
//!
//! The accumulator is the subtle part. It represents *progress toward the next
//! notch in the direction the user is currently turning*, so it is only
//! meaningful inside one continuous gesture. Carried across a reversal it gets
//! spent against the new direction and swallows the first notch the user asked
//! for; carried across a pause it leaks into an unrelated later gesture, a
//! different device, or a different binding. Both boundaries therefore reset
//! it.

use std::time::{Duration, Instant};

/// How long a gesture may pause before the leftover fraction is stale. Matches
/// the idle window the macOS gesture session uses to decide a turn has ended,
/// so all three platforms agree on when one zoom gesture stops.
const IDLE: Duration = Duration::from_millis(200);

/// Accumulates fractional magnification and yields whole notches.
pub(super) struct ZoomNotches {
    /// Progress toward the next notch, always in `(-1.0, 1.0)`.
    remainder: f64,
    /// When the last step arrived and whether it zoomed in, so a reversal or a
    /// pause can be told apart from a continuing turn.
    last: Option<(Instant, bool)>,
}

impl ZoomNotches {
    pub(super) const fn new() -> Self {
        Self {
            remainder: 0.0,
            last: None,
        }
    }

    /// Feed one step and take the whole notches it completes. `per_notch` is
    /// the magnification one notch is worth; `now` is injected so the reset
    /// boundaries are testable.
    pub(super) fn take(&mut self, magnification: f64, per_notch: f64, now: Instant) -> i32 {
        let zooming_in = magnification > 0.0;
        let continues = self
            .last
            .is_some_and(|(when, was_in)| was_in == zooming_in && now - when < IDLE);
        if !continues {
            self.remainder = 0.0;
        }
        self.remainder += magnification / per_notch;
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        self.last = Some((now, zooming_in));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "trunc() of an accumulator that never leaves (-1.0, 1.0) before this step"
        )]
        let notches = whole as i32;
        notches
    }
}

#[cfg(test)]
mod tests;

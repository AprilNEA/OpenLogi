//! Shared Zoom sensitivity normalization and non-Mac discrete response.

use openlogi_core::config::ThumbwheelSensitivity;

/// Preserve OpenLogi's default and both slider endpoints on the signed scale.
pub(in super::super) fn speed(sensitivity: ThumbwheelSensitivity) -> f64 {
    let value = f64::from(sensitivity.into_inner());
    let default = f64::from(ThumbwheelSensitivity::DEFAULT.into_inner());
    if value <= default {
        (value - default) / (default - f64::from(ThumbwheelSensitivity::MIN.into_inner()))
    } else {
        (value - default) / (f64::from(ThumbwheelSensitivity::MAX.into_inner()) - default)
    }
}

#[cfg(any(not(target_os = "macos"), test))]
mod response {
    use std::time::{Duration, Instant};

    use openlogi_core::config::ThumbwheelSensitivity;

    const INACTIVITY: Duration = Duration::from_millis(500);

    /// One signed displacement shared by both physical directions. The enclosing
    /// session/configuration owner discards this state on lifecycle changes.
    #[derive(Default)]
    pub(in super::super) struct Response {
        state: Option<Progress>,
    }

    struct Progress {
        sensitivity: ThumbwheelSensitivity,
        displacement: i64,
        last_event: Instant,
    }

    impl Response {
        /// Recognition consumes the first nonzero report; subsequent reports
        /// produce at most one discrete pulse and discard its overshoot.
        pub(in super::super) fn advance(
            &mut self,
            increments: i32,
            sensitivity: ThumbwheelSensitivity,
            now: Instant,
        ) -> bool {
            if increments == 0 {
                return false;
            }
            let progress = accelerated(increments);
            let Some(state) = self.state.as_mut().filter(|state| {
                state.sensitivity == sensitivity
                    && now.saturating_duration_since(state.last_event) <= INACTIVITY
            }) else {
                self.state = Some(Progress {
                    sensitivity,
                    displacement: 0,
                    last_event: now,
                });
                return false;
            };
            state.last_event = now;
            state.displacement += progress;
            if state.displacement.abs() > threshold(sensitivity) {
                state.displacement = 0;
                true
            } else {
                false
            }
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "per-report truncation is the response contract; i16 input bounds the result"
    )]
    fn accelerated(increments: i32) -> i64 {
        (f64::from(increments.abs()).powf(1.4) * f64::from(increments.signum())) as i64
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the contract truncates the interpolated threshold in the bounded 2..10 range"
    )]
    fn threshold(sensitivity: ThumbwheelSensitivity) -> i64 {
        let t = (1.0 - super::speed(sensitivity)) / 2.0;
        (2.0 * (1.0 - t) + 10.0 * t) as i64
    }
}

#[cfg(any(not(target_os = "macos"), test))]
pub(super) use response::Response;

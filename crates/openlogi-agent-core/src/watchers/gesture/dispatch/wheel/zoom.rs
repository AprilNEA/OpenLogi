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
    use openlogi_hid::thumbwheel::RotationStatus;

    const INACTIVITY: Duration = Duration::from_millis(500);

    /// One signed displacement shared by both physical directions. The enclosing
    /// session/configuration owner discards this state on lifecycle changes.
    #[derive(Default)]
    pub(in super::super) struct Response {
        state: Option<Progress>,
        // Preserve the received phase, including when repair changes admission.
        previous_phase: Option<RotationStatus>,
        pending_end: Option<Instant>,
    }

    struct Progress {
        sensitivity: ThumbwheelSensitivity,
        displacement: i64,
        last_event: Instant,
    }

    impl Response {
        /// Repair raw phases and lazily deliver the deferred end. This never
        /// emits output; only admitted progress reaches the response below.
        pub(in super::super) fn prepare(&mut self, phase: RotationStatus, now: Instant) -> bool {
            if self.pending_end.is_some_and(|deadline| now >= deadline) {
                self.state = None;
                self.pending_end = None;
            }
            let previous = self.previous_phase.unwrap_or(RotationStatus::Inactive);
            if matches!(
                (previous, phase),
                (RotationStatus::Start, RotationStatus::Start)
                    | (RotationStatus::Stop, RotationStatus::Stop)
            ) {
                // Suppression retains raw history and the original end deadline.
                return false;
            }
            self.previous_phase = Some(phase);
            let mapped = match (previous, phase) {
                (RotationStatus::Inactive, RotationStatus::Active) => RotationStatus::Start,
                (RotationStatus::Active, RotationStatus::Inactive) => RotationStatus::Stop,
                (_, phase) => phase,
            };
            match mapped {
                RotationStatus::Start => {
                    if self.pending_end.take().is_some() {
                        // A new start before deferred END continues as progress.
                        true
                    } else {
                        self.state = None;
                        false
                    }
                }
                RotationStatus::Active => true,
                RotationStatus::Stop => {
                    self.pending_end = Some(now + INACTIVITY);
                    false
                }
                RotationStatus::Inactive => false,
            }
        }

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

    fn accelerated(increments: i32) -> i64 {
        i64::from(openlogi_core::binding::accelerate_report_delta(increments))
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

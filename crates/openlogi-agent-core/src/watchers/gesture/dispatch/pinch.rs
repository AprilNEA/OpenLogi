//! Convert raw motion to magnification within a single held pinch.

use std::time::{Duration, Instant};

/// Ignore timing gaps after the mouse has stopped. Bound very short gaps
/// because several reports may arrive together from the receiver.
const IDLE_GAP: Duration = Duration::from_millis(80);
const MIN_REPORT_INTERVAL: Duration = Duration::from_millis(4);
const RAMP_START: f64 = 3_500.0;
const RAMP_END: f64 = 13_500.0;
const MAX_GAIN: f64 = 2.5;

#[derive(Default)]
pub(super) struct PinchRamp {
    previous: Option<(Instant, i16)>,
}

impl PinchRamp {
    pub(super) fn advance(&mut self, dy: i16, captured_at: Instant) -> f64 {
        let direction = dy.signum();
        let previous = self.previous.replace((captured_at, direction));
        let gain = previous
            .and_then(|(previous_at, previous_direction)| {
                let elapsed = captured_at.saturating_duration_since(previous_at);
                (previous_direction == direction && elapsed < IDLE_GAP).then(|| {
                    let speed =
                        f64::from(dy).abs() / elapsed.max(MIN_REPORT_INTERVAL).as_secs_f64();
                    let ramp = ((speed - RAMP_START) / (RAMP_END - RAMP_START)).clamp(0.0, 1.0);
                    1.0 + (MAX_GAIN - 1.0) * ramp
                })
            })
            .unwrap_or(1.0);

        // Positive raw movement is toward the user. Cap each update even if
        // the hardware reports an unusually large delta.
        (-f64::from(dy) * gain / 1200.0).clamp(-0.15, 0.15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerates_quick_motion_and_keeps_slow_motion_precise() {
        let now = Instant::now();
        let mut ramp = PinchRamp::default();
        assert_eq!(ramp.advance(-30, now), 0.025);
        assert_eq!(ramp.advance(-30, now + Duration::from_millis(20)), 0.025);
        assert!(ramp.advance(-30, now + Duration::from_millis(25)) > 0.025);
        assert_eq!(ramp.advance(-30, now + Duration::from_millis(45)), 0.025);
    }

    #[test]
    fn reversal_and_pause_start_at_base_speed() {
        let now = Instant::now();
        let mut ramp = PinchRamp::default();
        ramp.advance(-60, now);
        assert!(ramp.advance(-60, now + Duration::from_millis(5)) > 0.05);
        assert_eq!(ramp.advance(60, now + Duration::from_millis(10)), -0.05);
        assert!(ramp.advance(60, now + Duration::from_millis(15)) < -0.05);
        assert_eq!(ramp.advance(60, now + Duration::from_millis(100)), -0.05);
    }

    #[test]
    fn caps_large_or_batched_reports() {
        let now = Instant::now();
        let mut ramp = PinchRamp::default();
        assert_eq!(ramp.advance(i16::MIN, now), 0.15);
        assert_eq!(ramp.advance(i16::MIN, now), 0.15);
        assert_eq!(ramp.advance(i16::MAX, now), -0.15);
    }
}

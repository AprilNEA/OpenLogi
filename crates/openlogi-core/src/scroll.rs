//! Platform-neutral scroll distances, and the speed curve of a momentum
//! glide ([`GlideCurve`]).

use std::time::Duration;

use crate::config::GlideShape;

/// A signed two-axis scroll distance with an explicit unit.
///
/// Positive horizontal values scroll right; positive vertical values scroll
/// up. Keeping pixels distinct from standard wheel ticks prevents a smooth
/// scrolling runtime from accidentally interpolating or accumulating unlike
/// quantities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollDelta {
    /// Pixel-precise scrolling, as reported by continuous macOS wheel events.
    Pixels {
        /// Horizontal distance; positive scrolls right.
        x: f64,
        /// Vertical distance; positive scrolls up.
        y: f64,
    },
    /// Standard wheel ticks. One tick is one detent, represented by 120
    /// high-resolution wheel units on Linux and Windows.
    WheelTicks {
        /// Horizontal distance; positive scrolls right.
        x: f64,
        /// Vertical distance; positive scrolls up.
        y: f64,
    },
}

impl ScrollDelta {
    /// Construct a pixel-precise scroll distance.
    #[must_use]
    pub const fn pixels(x: f64, y: f64) -> Self {
        Self::Pixels { x, y }
    }

    /// Construct a scroll distance in standard wheel ticks.
    #[must_use]
    pub const fn wheel_ticks(x: f64, y: f64) -> Self {
        Self::WheelTicks { x, y }
    }

    /// Return the signed horizontal distance in this value's unit.
    #[must_use]
    pub const fn x(self) -> f64 {
        match self {
            Self::Pixels { x, .. } | Self::WheelTicks { x, .. } => x,
        }
    }

    /// Return the signed vertical distance in this value's unit.
    #[must_use]
    pub const fn y(self) -> f64 {
        match self {
            Self::Pixels { y, .. } | Self::WheelTicks { y, .. } => y,
        }
    }

    /// Whether both components are finite numbers suitable for interpolation.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x().is_finite() && self.y().is_finite()
    }

    /// Flip the sign of the vertical component, preserving the unit and the
    /// horizontal component.
    #[must_use]
    pub const fn negate_y(self) -> Self {
        match self {
            Self::Pixels { x, y } => Self::Pixels { x, y: -y },
            Self::WheelTicks { x, y } => Self::WheelTicks { x, y: -y },
        }
    }
}

/// One momentum glide: its speed falls from `start_speed` to zero as
/// `(1 − t/T)^p` over `T = τ·(p + 1)`, so it covers `D = start_speed · τ`
/// whatever its [`GlideShape`] `p`. Evaluated from elapsed time, never per
/// frame, so it lands on the same stop at any frame rate.
///
/// The agent glides by it and the settings draw it, so both show one curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlideCurve {
    start_speed: f64,
    exponent: f64,
    /// `τ`, in seconds.
    length: f64,
}

impl GlideCurve {
    /// A glide starting at `start_speed` (unsigned, units per second) with
    /// time constant `length`.
    #[must_use]
    pub const fn new(start_speed: f64, shape: GlideShape, length: Duration) -> Self {
        Self {
            start_speed,
            exponent: shape.exponent(),
            length: length.as_secs_f64(),
        }
    }

    /// The speed it starts at.
    #[must_use]
    pub const fn start_speed(&self) -> f64 {
        self.start_speed
    }

    /// How long it lasts: `τ·(p + 1)`.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.length * (self.exponent + 1.0))
    }

    /// The whole distance it covers: `start_speed · τ`.
    #[must_use]
    pub fn distance(&self) -> f64 {
        self.start_speed * self.length
    }

    /// The share of its duration left at `elapsed`, `1.0` down to `0.0`.
    fn remaining(&self, elapsed: Duration) -> f64 {
        // Compared as a `Duration`, so the curve ends exactly where
        // `duration` says, nanosecond rounding included.
        let duration = self.duration();
        if elapsed >= duration {
            return 0.0;
        }
        (1.0 - elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    }

    /// Its speed `elapsed` after it began.
    #[must_use]
    pub fn speed_at(&self, elapsed: Duration) -> f64 {
        self.start_speed * self.remaining(elapsed).powf(self.exponent)
    }

    /// The distance it has covered `elapsed` after it began; exactly
    /// [`Self::distance`] from [`Self::duration`] on.
    #[must_use]
    pub fn position_at(&self, elapsed: Duration) -> f64 {
        self.distance() * (1.0 - self.remaining(elapsed).powf(self.exponent + 1.0))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{GlideCurve, ScrollDelta};
    use crate::config::GlideShape;

    #[test]
    fn units_remain_distinct() {
        assert_ne!(
            ScrollDelta::pixels(1.0, -2.0),
            ScrollDelta::wheel_ticks(1.0, -2.0)
        );
    }

    #[test]
    fn rejects_non_finite_components() {
        assert!(!ScrollDelta::pixels(f64::NAN, 0.0).is_finite());
        assert!(!ScrollDelta::wheel_ticks(0.0, f64::INFINITY).is_finite());
        assert!(ScrollDelta::wheel_ticks(0.25, -1.0).is_finite());
    }

    #[test]
    fn negate_y_flips_only_the_vertical_component() {
        assert_eq!(
            ScrollDelta::wheel_ticks(3.0, 1.0).negate_y(),
            ScrollDelta::wheel_ticks(3.0, -1.0)
        );
        assert_eq!(
            ScrollDelta::pixels(-2.0, -5.0).negate_y(),
            ScrollDelta::pixels(-2.0, 5.0)
        );
    }

    #[test]
    fn every_shape_covers_the_same_distance_over_its_own_duration() {
        let length = Duration::from_millis(400);
        for shape in GlideShape::ALL {
            let curve = GlideCurve::new(3000.0, shape, length);
            let duration = curve.duration();
            let expected = 0.4 * (shape.exponent() + 1.0);
            assert!(
                (duration.as_secs_f64() - expected).abs() < 1e-9,
                "{shape:?}"
            );
            assert!((curve.distance() - 1200.0).abs() < 1e-9, "{shape:?}");
            assert!(
                (curve.position_at(duration) - 1200.0).abs() < 1e-9,
                "{shape:?}"
            );
            assert!((curve.position_at(duration * 2) - 1200.0).abs() < 1e-9);
            assert!((curve.speed_at(Duration::ZERO) - 3000.0).abs() < 1e-9);
            assert!(curve.speed_at(duration).abs() < 1e-9, "{shape:?}");
        }
    }

    #[test]
    fn the_distance_does_not_depend_on_the_frame_rate() {
        let curve = GlideCurve::new(5000.0, GlideShape::Smooth, Duration::from_millis(400));
        let total = |frame: Duration| {
            let (mut at, mut emitted, mut sum) = (Duration::ZERO, 0.0, 0.0);
            while at < curve.duration() + frame {
                let position = curve.position_at(at);
                sum += position - emitted;
                emitted = position;
                at += frame;
            }
            sum
        };
        let at_60 = total(Duration::from_micros(16_667));
        let at_120 = total(Duration::from_micros(8_333));
        assert!((at_60 - at_120).abs() < 1e-6, "{at_60} {at_120}");
        assert!((at_60 - curve.distance()).abs() < 1e-6);
    }

    #[test]
    fn a_sharper_shape_holds_its_speed_longer() {
        let length = Duration::from_millis(400);
        let at = Duration::from_millis(200);
        let speed = |shape| GlideCurve::new(1000.0, shape, length).speed_at(at);
        assert!(speed(GlideShape::Sharp) > speed(GlideShape::Linear));
        assert!(speed(GlideShape::Linear) > speed(GlideShape::Smooth));
        assert!(speed(GlideShape::Smooth) > speed(GlideShape::Floaty));
    }
}

//! Platform-neutral scroll distances.

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
}

/// Wheel rate threshold (in ticks/second) below which scrolling is considered
/// slow/deliberate and receives no acceleration (gain = 1.0).
pub const SCROLL_ACCEL_REFERENCE_RATE: f64 = 4.0;

/// Scaling denominator for the smooth monotonic gain curve.
pub const SCROLL_ACCEL_CURVE_SCALE: f64 = 8.0;

/// Pure scroll acceleration curve calculation.
///
/// Computes acceleration gain multiplier given an estimated physical wheel rate
/// (ticks/second), acceleration strength factor (0.2 to 2.0), and max gain bound (1.0 to 3.0).
#[must_use]
pub fn compute_acceleration_gain(
    estimated_rate: f64,
    acceleration_factor: f64,
    max_gain: f64,
) -> f64 {
    if max_gain <= 1.0
        || !estimated_rate.is_finite()
        || estimated_rate <= 0.0
        || !acceleration_factor.is_finite()
        || acceleration_factor <= 0.0
    {
        return 1.0;
    }

    let excess_speed = (estimated_rate - SCROLL_ACCEL_REFERENCE_RATE).max(0.0);
    if excess_speed == 0.0 {
        return 1.0;
    }

    let curve = excess_speed / (excess_speed + SCROLL_ACCEL_CURVE_SCALE);
    let gain = 1.0 + (max_gain - 1.0) * curve * acceleration_factor;

    gain.clamp(1.0, max_gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceleration_gain_returns_unity_for_slow_rate_or_disabled() {
        assert_eq!(compute_acceleration_gain(2.0, 1.0, 2.5), 1.0);
        assert_eq!(compute_acceleration_gain(4.0, 1.0, 2.5), 1.0);
        assert_eq!(compute_acceleration_gain(10.0, 1.0, 1.0), 1.0);
        assert_eq!(compute_acceleration_gain(10.0, 0.0, 2.5), 1.0);
        assert_eq!(compute_acceleration_gain(f64::NAN, 1.0, 2.5), 1.0);
    }

    #[test]
    fn acceleration_gain_increases_monotonically_and_clamps_to_max() {
        let g1 = compute_acceleration_gain(8.0, 1.0, 2.5);
        let g2 = compute_acceleration_gain(16.0, 1.0, 2.5);
        let g3 = compute_acceleration_gain(1000.0, 1.0, 2.5);
        let g4 = compute_acceleration_gain(1000.0, 2.0, 2.5);

        assert!(g1 > 1.0);
        assert!(g2 > g1);
        assert!(g3 > g2);
        assert_eq!(g4, 2.5, "higher strength at high rate clamps to max_gain");
    }

    #[test]
    fn acceleration_gain_strength_scales_curve() {
        let low_strength = compute_acceleration_gain(12.0, 0.5, 2.5);
        let high_strength = compute_acceleration_gain(12.0, 1.5, 2.5);

        assert!(high_strength > low_strength);
        assert!(low_strength > 1.0);
        assert!(high_strength <= 2.5);
    }
    use super::ScrollDelta;

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
}

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

#[cfg(test)]
mod tests {
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

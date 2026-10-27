//! Platform-neutral scroll distances.

/// Notches a mouse wheel travels per inch of on-screen distance at unit gain,
/// as measured from recorded Options+ event traces.
const WHEEL_NOTCHES_PER_INCH: f64 = 7.714_285_714_285_714;

/// The screen resolution wheel distances are measured against, in pixels per
/// inch. Pixel distances divided by it give the inches a wheel speed model
/// works in.
pub const WHEEL_REFERENCE_DPI: f64 = 100.0;

/// Pixels one wheel notch scrolls at unit gain (about 12.96).
///
/// The one owner of how far a notch is: the main-wheel model scales its pixel
/// output from it and the injector converts model pixels back to wheel ticks
/// on platforms without pixel output, so the two cannot drift apart.
pub const WHEEL_PIXELS_PER_NOTCH: f64 = WHEEL_REFERENCE_DPI / WHEEL_NOTCHES_PER_INCH;

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

#[cfg(test)]
mod tests {
    use super::{ScrollDelta, WHEEL_PIXELS_PER_NOTCH};

    #[test]
    fn a_notch_is_about_thirteen_pixels() {
        assert!(
            (WHEEL_PIXELS_PER_NOTCH - 12.963).abs() < 1e-3,
            "{WHEEL_PIXELS_PER_NOTCH}"
        );
    }

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
}

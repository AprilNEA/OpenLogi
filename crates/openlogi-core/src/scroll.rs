//! Platform-neutral scroll distances, and the glide model of momentum
//! scrolling ([`crate::config::MomentumScrolling`]).

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

/// Wheel speed, in notches per second, at which the glide runs at the
/// configured glide speed.
pub const MOMENTUM_REFERENCE_SPEED: f64 = 100.0;
/// Below this wheel speed the glide gives no output; it fades in up to
/// [`MOMENTUM_FADE_FULL`], so the wheel's slow trickle at the end of a spin
/// does not keep the page creeping.
pub const MOMENTUM_FADE_ZERO: f64 = 20.0;
/// See [`MOMENTUM_FADE_ZERO`].
pub const MOMENTUM_FADE_FULL: f64 = 45.0;
/// Below this glide speed (px/s) the coast firms up toward its tail rate, as
/// Apple's momentum does (WebKit blends 0.975→0.91 per 8 ms).
pub const MOMENTUM_TAIL_SPEED: f64 = 100.0;
/// The tail's time constant relative to the main coast's: Apple's tail
/// decays at 0.91 per 8 ms against 0.962 mid-glide, ln 0.962 / ln 0.91 ≈ 0.41.
pub const MOMENTUM_TAIL_RATIO: f64 = 0.41;
/// The glide ends below this speed, px/s — about where Apple's momentum stops.
pub const MOMENTUM_END_SPEED: f64 = 6.0;

/// Glide speed, in pixels per second, for a free-spinning wheel turning at
/// `wheel_speed` notches per second.
///
/// `glide_speed` px/s at [`MOMENTUM_REFERENCE_SPEED`], scaled by
/// `(wheel_speed / reference) ^ (response_tenths / 10)`. A response above 1
/// makes a hard flick glide disproportionately faster than a gentle one.
#[must_use]
pub fn momentum_output(wheel_speed: f64, glide_speed: u16, response_tenths: u8) -> f64 {
    use crate::config::MomentumScrolling as M;
    if !wheel_speed.is_finite() || wheel_speed <= MOMENTUM_FADE_ZERO {
        return 0.0;
    }
    let speed = f64::from(glide_speed.clamp(M::GLIDE_SPEED_MIN, M::GLIDE_SPEED_MAX));
    let exponent = f64::from(response_tenths.clamp(M::RESPONSE_MIN, M::RESPONSE_MAX)) / 10.0;
    let fade = ((wheel_speed - MOMENTUM_FADE_ZERO) / (MOMENTUM_FADE_FULL - MOMENTUM_FADE_ZERO))
        .clamp(0.0, 1.0);
    let fade = fade * fade * (3.0 - 2.0 * fade);
    speed * (wheel_speed / MOMENTUM_REFERENCE_SPEED).powf(exponent) * fade
}

/// Decay rate, per second, of the glide at full friction: it halves about
/// every 115 ms.
pub const MOMENTUM_FRICTION_MAX_RATE: f64 = 6.0;

/// The share of its wheel-driven speed a glide keeps `since_peak_s` seconds
/// after its flick peaked, under `friction_percent` friction: 1 without
/// friction, decaying exponentially with it — so the glide slows on its own,
/// as a Magic Mouse's does, however long the wheel spins on.
#[must_use]
pub fn momentum_friction(friction_percent: u8, since_peak_s: f64) -> f64 {
    use crate::config::MomentumScrolling as M;
    let rate = MOMENTUM_FRICTION_MAX_RATE * f64::from(friction_percent.min(M::FRICTION_MAX))
        / f64::from(M::FRICTION_MAX);
    (-rate * since_peak_s.max(0.0)).exp()
}

/// Time constant, in seconds, a rise of the glide's speed eases in with: a
/// faster wheel or a repeated flick's speed-up is ~90% in after 70 ms
/// instead of jumping in one frame.
pub const MOMENTUM_RISE_S: f64 = 0.03;

/// One frame of the glide's speed, px/s, following the wheel-driven
/// `target` ([`momentum_output`]) from the `previous` speed over
/// `elapsed_s`. A rise eases in ([`MOMENTUM_RISE_S`]), so the wobble of the
/// wheel's measured speed and a re-flick's speed-up never jump the page. A
/// fall is filtered: the speed decays no faster than Apple's trackpad
/// momentum — time constant `glide_ms` mid-glide (0 = follow the wheel
/// exactly), firming up toward the tail rate ([`MOMENTUM_TAIL_RATIO`]) below
/// [`MOMENTUM_TAIL_SPEED`] — so a finger catching the wheel to flick again
/// does not drag the glide down, and its end tapers out.
#[must_use]
pub fn momentum_coast(previous: f64, target: f64, elapsed_s: f64, glide_ms: u16) -> f64 {
    const FRAME_S: f64 = 0.008;
    if target > previous {
        return previous + (target - previous) * (1.0 - (-elapsed_s / MOMENTUM_RISE_S).exp());
    }
    if glide_ms == 0 {
        return target;
    }
    let tau = f64::from(glide_ms) / 1000.0;
    let alpha_main = (-FRAME_S / tau).exp();
    let alpha_tail = (-FRAME_S / (tau * MOMENTUM_TAIL_RATIO)).exp();
    let weak = ((MOMENTUM_TAIL_SPEED - previous) / MOMENTUM_TAIL_SPEED).clamp(0.0, 1.0);
    let alpha = alpha_main - (alpha_main - alpha_tail) * weak;
    let coasted = previous * alpha.powf(elapsed_s / FRAME_S);
    target.max(coasted)
}

#[cfg(test)]
mod tests {
    use super::{
        MOMENTUM_FADE_ZERO, ScrollDelta, momentum_coast, momentum_friction, momentum_output,
    };

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
    fn the_glide_grows_with_wheel_speed_and_fades_out_slowly() {
        assert!(momentum_output(MOMENTUM_FADE_ZERO, 1400, 15).abs() < f64::EPSILON);
        assert!((momentum_output(100.0, 1400, 15) - 1400.0).abs() < 1e-9);
        let slow = momentum_output(60.0, 1400, 15);
        let fast = momentum_output(200.0, 1400, 15);
        assert!(slow < 1400.0 && fast > 2.0 * 1400.0, "{slow} {fast}");
        assert!(momentum_output(f64::NAN, 1400, 15).abs() < f64::EPSILON);
    }

    #[test]
    fn friction_slows_the_glide_over_time_and_off_leaves_it_alone() {
        assert!((momentum_friction(0, 5.0) - 1.0).abs() < f64::EPSILON);
        assert!((momentum_friction(50, 0.0) - 1.0).abs() < f64::EPSILON);
        let light = momentum_friction(20, 0.5);
        let strong = momentum_friction(100, 0.5);
        assert!(strong < light && light < 1.0, "{strong} {light}");
        // Out-of-range values clamp to the strongest friction.
        assert!((momentum_friction(255, 0.5) - strong).abs() < f64::EPSILON);
    }

    #[test]
    fn the_coast_eases_rises_and_never_drops_faster_than_its_decay() {
        // A faster wheel eases in: most of the way within one 60 Hz frame,
        // all of it within a few.
        let risen = momentum_coast(100.0, 900.0, 0.016, 206);
        assert!(risen > 400.0 && risen < 900.0, "{risen}");
        let settled = (0..6).fold(100.0, |v, _| momentum_coast(v, 900.0, 0.016, 206));
        assert!(settled > 850.0 && settled <= 900.0, "{settled}");
        // With the wheel gone the glide decays, but not to zero at once…
        let coasted = momentum_coast(1000.0, 0.0, 0.016, 206);
        assert!(coasted > 900.0 && coasted < 1000.0, "{coasted}");
        // …unless the coast is off.
        assert!(momentum_coast(1000.0, 0.0, 0.016, 0).abs() < f64::EPSILON);
    }
}

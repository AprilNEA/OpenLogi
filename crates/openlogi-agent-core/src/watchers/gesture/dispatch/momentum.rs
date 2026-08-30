//! Decaying scroll momentum for the synthesized two-finger scroll.
//!
//! The recognizer stops at lift-off; the glide a trackpad shows afterwards
//! lives here. The cadence and the `×0.955` exponential mirror the Options+
//! agent's `processWheelInertia` (reverse-engineered from its unstripped
//! binary); the low-speed end fades out asymptotically instead — Options+
//! collapses its tail with a `|v|/(|v|+v₀)` term, which hardware testing
//! showed as a jolt, and a content-velocity gate keeps deliberate slow
//! scrolls dead in place.
//!
//! The tail posts as ordinary scroll-phase `Changed` frames continuing the
//! stroke's stream — on-session probing showed macOS 27 ignores synthesized
//! events that carry a momentum phase (four injection recipes, including Mac
//! Mouse Fix's production one, all inert), while the pad's own firmware
//! "momentum" is simply more unphased wheel deltas after lift. Plain deltas
//! are the one shape proven to scroll.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openlogi_core::scroll::ScrollDelta;

use super::super::TouchpadScrollTuning;

/// One momentum tick — the Options+ cadence (90.9 Hz).
const TICK: Duration = Duration::from_millis(11);
/// Seconds per tick, the delta multiplier for a per-second velocity.
const TICK_SECONDS: f64 = 0.011;
/// Velocity multiplier applied every tick, pure exponential: 0.955 per 11 ms
/// ≈ 0.97 per 60 Hz frame — between iOS `normal` and `fast` deceleration.
/// Deliberately no low-speed convergence term: hardware testing showed the
/// progressively harder brake it produces reads as a visible jolt right
/// before the stop, where the native glide just fades out.
const DECAY_PER_TICK: f64 = 0.955;
/// Where the tail loop stops, in content px/s — a tenth of a pixel per tick,
/// already beneath the pixel quantizer's rounding threshold. The visible
/// motion ends by fading through sub-pixel deltas into the quantizer's
/// residual carry, not by braking.
const STOP_PX_PER_S: f64 = 10.0;
/// Lift-off speed below which no momentum starts, in content px/s. A
/// deliberate slow scroll (≈ 40 mm/s of finger travel at the default gain)
/// stays put; anything brisker glides. Scales with the device's sensitivity,
/// as Options+ scales its gate by the user's scroll-speed setting.
const START_PX_PER_S: f64 = 1_000.0;

/// One running momentum tail. Dropping the handle does not stop it — the
/// dispatcher owns the lifecycle explicitly through [`Self::stop`], and the
/// thread always terminates on its own once the tail decays.
#[derive(Debug)]
pub(super) struct TouchpadMomentum {
    stop: Arc<AtomicBool>,
}

impl TouchpadMomentum {
    /// Start a decaying tail from the lift-off velocity of a finished scroll
    /// stroke, in micrometres of centroid travel per second. Returns `None`
    /// when the lift-off was too slow to glide, or the thread could not
    /// spawn (in which case scrolling simply stops at lift, as before).
    pub(super) fn start(
        tuning: TouchpadScrollTuning,
        exit_velocity_um_per_s: (f64, f64),
    ) -> Option<Self> {
        let mut velocity = glide_velocity(tuning, exit_velocity_um_per_s)?;

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("touchpad-momentum".to_string())
            .spawn(move || run(&mut velocity, &flag));
        if spawned.is_err() {
            // Scrolling simply stops at lift, as it did before momentum.
            return None;
        }

        tracing::debug!(?velocity, "touchpad scroll momentum started");
        Some(Self { stop })
    }

    /// Stop the tail on the next tick (≤ one tick of latency): the thread,
    /// which owns the phase machine, posts the terminal zero-delta end.
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// `velocity` already lives in content pixels per second with the device's
/// tuning applied — the tuning must not be re-applied on the way out, so the
/// tail posts through the inject layer directly.
fn run(velocity: &mut (f64, f64), stop: &AtomicBool) {
    let mut ticks = 0_u32;
    loop {
        // The per-tick distance comes from the velocity *before* the decay,
        // matching the Options+ tick shape (velocity is per-tick distance
        // divided by the tick length). Wheel-class frames need no phase
        // closure: the stream ends by simply stopping.
        openlogi_inject::post_touchpad_scroll(
            ScrollDelta::pixels(velocity.0 * TICK_SECONDS, velocity.1 * TICK_SECONDS),
            None,
        );
        ticks += 1;

        if stop.load(Ordering::Acquire) {
            break;
        }
        if speed(*velocity) <= STOP_PX_PER_S {
            break;
        }
        velocity.0 *= DECAY_PER_TICK;
        velocity.1 *= DECAY_PER_TICK;
        std::thread::sleep(TICK);
    }
    tracing::debug!(
        ticks,
        speed = speed(*velocity),
        "touchpad momentum tail ended"
    );
}

/// The tail's starting content velocity, or `None` when the lift-off was
/// too slow (or too non-finite) to glide.
fn glide_velocity(
    tuning: TouchpadScrollTuning,
    exit_velocity_um_per_s: (f64, f64),
) -> Option<(f64, f64)> {
    let initial = content_delta(tuning, &exit_velocity_um_per_s);
    let velocity = (initial.x(), initial.y());
    (velocity.0.is_finite()
        && velocity.1.is_finite()
        && speed(velocity) > START_PX_PER_S.max(STOP_PX_PER_S))
    .then_some(velocity)
}

/// One shared µm→content-pixel projection for per-second velocities.
#[expect(
    clippy::cast_possible_truncation,
    reason = "sub-micrometre truncation of a per-second velocity is imperceptible"
)]
fn content_delta(
    tuning: TouchpadScrollTuning,
    velocity_um_per_s: &(f64, f64),
) -> openlogi_core::scroll::ScrollDelta {
    tuning.content_delta(velocity_um_per_s.0 as i64, velocity_um_per_s.1 as i64)
}

fn speed(velocity: (f64, f64)) -> f64 {
    velocity.0.hypot(velocity.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_shrinks_the_tail_and_preserves_direction() {
        let mut velocity = (3000.0, -4000.0);
        let magnitude = speed(velocity);
        velocity.0 *= DECAY_PER_TICK;
        velocity.1 *= DECAY_PER_TICK;

        let shrunk = speed(velocity);
        assert!((magnitude - shrunk - magnitude * (1.0 - DECAY_PER_TICK)).abs() < 1e-9);
        // Direction survives: both components keep their sign and ratio.
        assert!((velocity.0 / velocity.1 - 3000.0 / -4000.0).abs() < 1e-12);
    }

    #[test]
    fn slow_lift_offs_never_glide() {
        let tuning = TouchpadScrollTuning::NEUTRAL;
        // 20 mm/s of finger travel is a deliberate placement, not a flick.
        assert_eq!(glide_velocity(tuning, (0.0, 20_000.0)), None);
        // 400 mm/s is a flick: 10 mm of travel per 25 ms frame.
        let glide = glide_velocity(tuning, (0.0, 400_000.0)).expect("a brisk lift-off glides");
        // Neutral tuning keeps the content-following mapping: downward
        // finger motion scrolls up in wheel convention, i.e. positive y.
        assert_eq!(glide, (0.0, 10_000.0));
    }

    #[test]
    fn inverted_tuning_flips_the_glide() {
        let plan = crate::capture_plan::DispatchPlan {
            config_key: "casa".to_string(),
            bindings: std::collections::BTreeMap::new(),
            gesture_bindings: std::collections::BTreeMap::new(),
            side_gesture_bindings: std::collections::BTreeMap::new(),
            thumbwheel_sensitivity: openlogi_core::config::ThumbwheelSensitivity::DEFAULT,
            touchpad_bindings: std::collections::BTreeMap::new(),
            touchpad_scroll_sensitivity: openlogi_core::config::TouchpadScrollSensitivity::DEFAULT,
            touchpad_scroll_inverted: true,
        };
        let glide = glide_velocity(TouchpadScrollTuning::from_plan(&plan), (400_000.0, 0.0))
            .expect("inversion must not gate off the glide");
        // Uninverted rightward travel maps to negative x; inversion flips it.
        assert_eq!(glide, (10_000.0, 0.0));
    }
}

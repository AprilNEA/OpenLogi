//! Decaying scroll momentum for the synthesized two-finger scroll.
//!
//! The recognizer stops at lift-off; the glide a trackpad shows afterwards
//! lives here. The cadence and the `×0.955` exponential mirror the Options+
//! agent's `processWheelInertia` (reverse-engineered from its unstripped
//! binary); the low-speed end fades out asymptotically instead — Options+
//! collapses its tail with a `|v|/(|v|+v₀)` term, which hardware testing
//! showed as a jolt — and a physical-velocity gate keeps deliberate slow
//! scrolls dead in place.
//!
//! The tail posts phase-less pixel deltas — on-session probing showed macOS
//! 27 ignores synthesized events that carry a momentum phase (four injection
//! recipes, including Mac Mouse Fix's production one, all inert), while the
//! pad's own firmware "momentum" is simply more unphased wheel deltas after
//! lift. Plain deltas are the one shape proven to scroll, and through the
//! session tap they land as exact per-pixel values. A wheel-class stream
//! needs no closure event: it ends by stopping.

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
/// Lift-off finger speed below which no momentum starts, in micrometres of
/// centroid travel per second: ≈ 40 mm/s, a deliberate placement rather than
/// a flick. Gated on the physical velocity — not the content velocity — so
/// the sensitivity setting changes how far a glide carries, never whether
/// one starts (at the content gate a MIN-sensitivity device could never
/// glide at all, a MAX one always would).
const START_UM_PER_S: f64 = 40_000.0;

/// One running momentum tail. Dropping the handle does not stop it — the
/// dispatcher owns the lifecycle explicitly through [`Self::stop`], and the
/// thread always terminates on its own once the tail decays.
#[derive(Debug)]
pub(super) struct TouchpadMomentum {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
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
        Some(Self {
            stop,
            thread: spawned.ok(),
        })
    }

    /// The join is load-bearing: it orders every remaining delta before the
    /// replacement output the caller posts next, never after it.
    pub(super) fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `velocity` already lives in content pixels per second with the device's
/// tuning applied — the tuning must not be re-applied on the way out, so the
/// tail posts through the inject layer directly.
fn run(velocity: &mut (f64, f64), stop: &AtomicBool) {
    let mut ticks = 0_u32;
    loop {
        // Bounds a stop to the one in-flight frame; the join orders it.
        if stop.load(Ordering::Acquire) {
            break;
        }
        // The per-tick distance comes from the velocity *before* the decay,
        // matching the Options+ tick shape (velocity is per-tick distance
        // divided by the tick length).
        openlogi_inject::post_touchpad_scroll(ScrollDelta::pixels(
            velocity.0 * TICK_SECONDS,
            velocity.1 * TICK_SECONDS,
        ));
        ticks += 1;

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
#[expect(
    clippy::cast_possible_truncation,
    reason = "sub-micrometre truncation of a per-second velocity is imperceptible"
)]
fn glide_velocity(
    tuning: TouchpadScrollTuning,
    exit_velocity_um_per_s: (f64, f64),
) -> Option<(f64, f64)> {
    if !exit_velocity_um_per_s.0.is_finite()
        || !exit_velocity_um_per_s.1.is_finite()
        || speed(exit_velocity_um_per_s) <= START_UM_PER_S
    {
        return None;
    }
    let initial = tuning.content_delta(
        exit_velocity_um_per_s.0 as i64,
        exit_velocity_um_per_s.1 as i64,
    );
    Some((initial.x(), initial.y()))
}

fn speed(velocity: (f64, f64)) -> f64 {
    velocity.0.hypot(velocity.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_tuning() -> TouchpadScrollTuning {
        TouchpadScrollTuning::from_plan(&neutral_plan(false))
    }

    fn neutral_plan(inverted: bool) -> crate::capture_plan::DispatchPlan {
        crate::capture_plan::DispatchPlan {
            config_key: "casa".to_string(),
            bindings: std::collections::BTreeMap::new(),
            gesture_bindings: std::collections::BTreeMap::new(),
            side_gesture_bindings: std::collections::BTreeMap::new(),
            thumbwheel_sensitivity: openlogi_core::config::ThumbwheelSensitivity::DEFAULT,
            touchpad_bindings: std::collections::BTreeMap::new(),
            touchpad_scroll_sensitivity: openlogi_core::config::TouchpadScrollSensitivity::DEFAULT,
            touchpad_scroll_inverted: inverted,
        }
    }

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
        // 20 mm/s of finger travel is a deliberate placement, not a flick.
        assert_eq!(glide_velocity(neutral_tuning(), (0.0, 20_000.0)), None);
        // 400 mm/s is a flick: 10 mm of travel per 25 ms frame.
        let glide =
            glide_velocity(neutral_tuning(), (0.0, 400_000.0)).expect("a brisk lift-off glides");
        // Neutral tuning keeps the content-following mapping: downward
        // finger motion scrolls up in wheel convention, i.e. positive y.
        assert_eq!(glide, (0.0, 10_000.0));
    }

    #[test]
    fn inverted_tuning_flips_the_glide() {
        let glide = glide_velocity(
            TouchpadScrollTuning::from_plan(&neutral_plan(true)),
            (400_000.0, 0.0),
        )
        .expect("inversion must not gate off the glide");
        // Uninverted rightward travel maps to negative x; inversion flips it.
        assert_eq!(glide, (10_000.0, 0.0));
    }

    #[test]
    fn the_gate_is_physical_so_sensitivity_cannot_disarm_it() {
        let max_sensitivity = crate::capture_plan::DispatchPlan {
            touchpad_scroll_sensitivity: openlogi_core::config::TouchpadScrollSensitivity::MAX,
            ..neutral_plan(false)
        };
        // A slow placement stays put even with the gain at maximum, and a
        // flick glides even at minimum — the gate reads the finger, not the
        // content speed the gain produces.
        assert_eq!(
            glide_velocity(
                TouchpadScrollTuning::from_plan(&max_sensitivity),
                (0.0, 20_000.0)
            ),
            None
        );
        let min_sensitivity = crate::capture_plan::DispatchPlan {
            touchpad_scroll_sensitivity: openlogi_core::config::TouchpadScrollSensitivity::MIN,
            ..neutral_plan(false)
        };
        assert!(
            glide_velocity(
                TouchpadScrollTuning::from_plan(&min_sensitivity),
                (0.0, 400_000.0)
            )
            .is_some()
        );
    }
}

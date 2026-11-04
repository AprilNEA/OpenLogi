//! The glide model of momentum scrolling: how fast a free spin sends the page
//! gliding. How the glide then slows is the shared [`GlideCurve`].

use openlogi_core::config::MomentumParams;
use openlogi_core::scroll::GlideCurve;

/// Wheel speed, in notches per second, of a gentle free spin: it glides off
/// at the pace the page was already scrolling (×1). Calibrated on the MX
/// Master 4 recordings in `free_spin_recordings.txt`, where gentle spins peak
/// around 70–115 notches/s and hard ones around 170–260.
const GENTLE_SPIN: f64 = 80.0;
/// Wheel speed of a hard spin, which glides [`FLICK_MAX`] times faster.
const HARD_SPIN: f64 = 250.0;
/// The flick response of the hardest spin.
const FLICK_MAX: f64 = 3.0;
/// How the flick response eases in between a gentle and a hard spin: above
/// 1, a mid-range spin gets clearly less than halfway to [`FLICK_MAX`].
const FLICK_GAMMA: f64 = 2.0;

/// The speed multiplier for a spin at `wheel_speed` notches per second: ×1
/// for a gentle spin rising to ×[`FLICK_MAX`] for a hard one, eased in.
pub(super) fn flick_response(wheel_speed: f64) -> f64 {
    let hardness = ((wheel_speed - GENTLE_SPIN) / (HARD_SPIN - GENTLE_SPIN)).clamp(0.0, 1.0);
    1.0 + (FLICK_MAX - 1.0) * hardness.powf(FLICK_GAMMA)
}

/// Time constant, in seconds, a rise of the glide's speed eases in with:
/// a faster spin, a repeat's speed-up or the launch over the ratchet pace is
/// ~90% in after 70 ms instead of jumping in one frame. The wheel's measured
/// speed wobbles 15–25% from frame to frame with the quantization of its
/// reports, and the flick response amplifies that, so a speed-up taken in one
/// frame snaps the page forward several times a second.
pub(super) const RISE_S: f64 = 0.03;

/// The pace, in pixels per second, a wheel turning at `wheel_speed`
/// notches per second scrolls the page at notch by notch, in the user's
/// `sensitivity` — where a glide launching off it eases up from.
pub(super) fn pace(wheel_speed: f64, sensitivity: f64, params: &MomentumParams) -> f64 {
    if !wheel_speed.is_finite() || wheel_speed <= 0.0 {
        return 0.0;
    }
    wheel_speed * params.pixels_per_notch() * sensitivity
}

/// The most repeated spins can multiply a glide.
const MAX_CHAIN_BOOST: f64 = 3.0;

/// The speed, in pixels per second, one spin of a wheel at `wheel_speed`
/// notches per second sends the page gliding at — the pace it was scrolling,
/// in the user's `sensitivity`, scaled by the flick response — capped at the
/// max glide speed.
pub(super) fn launch_speed(wheel_speed: f64, sensitivity: f64, params: &MomentumParams) -> f64 {
    let pace = pace(wheel_speed, sensitivity, params);
    if pace == 0.0 {
        return 0.0;
    }
    (pace * flick_response(wheel_speed)).min(params.max_glide_speed())
}

/// The speed-up after `chain` repeated spins, each multiplying the glide by
/// the repeat speed-up — like repeated trackpad flicks — up to
/// [`MAX_CHAIN_BOOST`]. It multiplies on top of the max glide speed, which
/// caps one spin, so repeating always has room to speed the page up.
pub(super) fn chain_boost(params: &MomentumParams, chain: u32) -> f64 {
    params
        .repeat_boost()
        .powi(i32::try_from(chain).unwrap_or(i32::MAX))
        .min(MAX_CHAIN_BOOST)
}

/// The glide curve from `speed`.
pub(super) fn curve(speed: f64, params: &MomentumParams) -> GlideCurve {
    GlideCurve::new(speed.max(0.0), params.glide_shape(), params.glide_length())
}

#[cfg(test)]
mod tests {
    use openlogi_core::config::MomentumScrolling;

    use super::{FLICK_MAX, HARD_SPIN, chain_boost, curve, flick_response, launch_speed};

    fn params() -> openlogi_core::config::MomentumParams {
        MomentumScrolling {
            enabled: true,
            ..MomentumScrolling::default()
        }
        .resolve()
        .expect("enabled")
    }

    #[test]
    fn a_gentle_spin_glides_as_it_scrolled_and_a_hard_one_three_times_faster() {
        assert!((flick_response(40.0) - 1.0).abs() < f64::EPSILON);
        assert!((flick_response(80.0) - 1.0).abs() < f64::EPSILON);
        assert!((flick_response(HARD_SPIN) - FLICK_MAX).abs() < f64::EPSILON);
        assert!((flick_response(1000.0) - FLICK_MAX).abs() < f64::EPSILON);
        // Eased in: halfway between gentle and hard is well short of ×2.
        let mid = flick_response(f64::midpoint(80.0, HARD_SPIN));
        assert!(mid > 1.0 && mid < 2.0, "{mid}");
    }

    #[test]
    fn a_gentle_launch_keeps_the_pace_the_page_scrolled_at() {
        let p = params();
        // 60 notches/s at 28 px a notch, unscaled.
        assert!((launch_speed(60.0, 1.0, &p) - 60.0 * 28.0).abs() < 1e-9);
        assert!((launch_speed(60.0, 2.0, &p) - 2.0 * 60.0 * 28.0).abs() < 1e-9);
        assert!(launch_speed(f64::NAN, 1.0, &p).abs() < f64::EPSILON);
        assert!(launch_speed(-5.0, 1.0, &p).abs() < f64::EPSILON);
    }

    #[test]
    fn one_spin_never_launches_above_the_max_glide_speed() {
        let p = params();
        assert!((launch_speed(1e6, 1.0, &p) - p.max_glide_speed()).abs() < f64::EPSILON);
        assert!((curve(1000.0, &p).start_speed() - 1000.0).abs() < f64::EPSILON);
        assert!((curve(1000.0, &p).distance() - 1000.0 * 0.4).abs() < 1e-9);
    }

    #[test]
    fn repeated_spins_compound_up_to_three_times() {
        let p = params();
        assert!((chain_boost(&p, 0) - 1.0).abs() < f64::EPSILON);
        assert!((chain_boost(&p, 1) - 1.35).abs() < 1e-9);
        assert!((chain_boost(&p, 2) - 1.35 * 1.35).abs() < 1e-9);
        assert!((chain_boost(&p, 10) - 3.0).abs() < f64::EPSILON);
    }
}

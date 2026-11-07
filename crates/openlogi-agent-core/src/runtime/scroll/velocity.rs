//! Continuous wheel velocity, with an exact integral between output frames.
//!
//! Active input tracks a target exponentially. Only a zero target starts the
//! configured coast curve, so coast settings never bias a held wheel speed.

use std::time::{Duration, Instant};

use openlogi_core::config::MomentumParams;
use openlogi_core::scroll::GlideCurve;

/// Velocity changes settle by about 90% in 140ms. This damps quantized
/// wheel-speed measurements without tying the response to output cadence.
const RESPONSE_S: f64 = 0.06;

enum Response {
    Tracking { target: f64, difference: f64 },
    Coasting(GlideCurve),
}

impl Response {
    fn new(initial: f64, target: f64, params: &MomentumParams) -> Self {
        if target > 0.0 {
            Self::Tracking {
                target,
                difference: initial - target,
            }
        } else {
            Self::Coasting(super::glide::curve(initial, params))
        }
    }

    fn target(&self) -> f64 {
        match self {
            Self::Tracking { target, .. } => *target,
            Self::Coasting(_) => 0.0,
        }
    }

    fn speed(&self, elapsed: Duration) -> f64 {
        match self {
            Self::Tracking { target, difference } => {
                target + difference * (-elapsed.as_secs_f64() / RESPONSE_S).exp()
            }
            Self::Coasting(curve) => curve.speed_at(elapsed),
        }
    }

    fn position(&self, elapsed: Duration) -> f64 {
        match self {
            Self::Tracking { target, difference } => {
                let seconds = elapsed.as_secs_f64();
                target * seconds - difference * RESPONSE_S * (-seconds / RESPONSE_S).exp_m1()
            }
            Self::Coasting(curve) => curve.position_at(elapsed),
        }
    }
}

/// A velocity trajectory whose target can change between output frames.
pub(super) struct Velocity {
    response: Response,
    began: Instant,
    /// Distance already transferred from this response into output or pending.
    sampled: f64,
    /// Distance from replaced responses that has not reached an output frame.
    pending: f64,
}

impl Velocity {
    /// Start from `initial`, tracking `target` or coasting when it is zero.
    pub(super) fn new(initial: f64, target: f64, params: &MomentumParams, at: Instant) -> Self {
        Self {
            response: Response::new(initial, target, params),
            began: at,
            sampled: 0.0,
            pending: 0.0,
        }
    }

    /// Current unsigned velocity, independent of when frames are emitted.
    pub(super) fn speed(&self, at: Instant) -> f64 {
        self.response
            .speed(at.saturating_duration_since(self.began))
    }

    /// Change target without dropping distance or changing instantaneous speed.
    #[expect(
        clippy::float_cmp,
        reason = "Only an exactly unchanged target preserves the existing trajectory"
    )]
    pub(super) fn retarget(&mut self, target: f64, params: &MomentumParams, at: Instant) {
        if self.response.target() == target {
            return;
        }
        let initial = self.speed(at);
        self.pending += self.sample(at);
        self.response = Response::new(initial, target, params);
        self.began = at;
        self.sampled = 0.0;
    }

    /// Take all distance since the previous frame, including replaced responses.
    pub(super) fn advance(&mut self, at: Instant) -> f64 {
        std::mem::take(&mut self.pending) + self.sample(at)
    }

    /// Whether a zero-target coast has finished and its distance was emitted.
    pub(super) fn finished(&self, at: Instant) -> bool {
        matches!(&self.response, Response::Coasting(curve)
            if at.saturating_duration_since(self.began) >= curve.duration()
                && self.pending <= 0.0 && self.sampled >= curve.distance())
    }

    fn sample(&mut self, at: Instant) -> f64 {
        let position = self
            .response
            .position(at.saturating_duration_since(self.began));
        let distance = (position - self.sampled).max(0.0);
        self.sampled = self.sampled.max(position);
        distance
    }
}

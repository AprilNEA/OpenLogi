//! Turns a dialpad's diverted rotary increments into actions.
//!
//! Each rotary control (dial, roller) binds its two directions as separate
//! [`ButtonId`]s. A continuous-scroll binding scrolls in proportion to the
//! rotation; any other action fires once per accumulated step, every step
//! — a dial is how a creative app nudges a value, so a fast turn must not be
//! throttled the way the thumb wheel's flick guard throttles it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, Rotary};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;

/// Scroll notches one full revolution produces at default sensitivity, and the
/// discrete steps it is divided into — about a mouse wheel's worth.
const STEPS_PER_REVOLUTION: f64 = 24.0;

/// Idle gap after which a partly accumulated step is forgotten, so a slow
/// accidental nudge does not complete a step much later.
const STEP_DECAY: Duration = Duration::from_millis(400);

/// Bound synchronous action dispatch for one captured event.
const MAX_FIRES_PER_EVENT: u32 = 20;

/// What one rotation event should produce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RotaryOutput {
    /// Below a step, or bound to nothing.
    Idle,
    /// Continuous scroll for a scroll binding.
    Scroll(ScrollDelta),
    /// Fire the bound action this many times.
    Fire(u32),
}

/// Partial-step state per rotary direction of one device.
#[derive(Default)]
pub(super) struct RotaryAccumulators {
    directions: HashMap<ButtonId, Progress>,
}

/// Progress toward the next step of one direction, owned by the exact action
/// and sensitivity that earned it.
struct Progress {
    action: Action,
    sensitivity: ThumbwheelSensitivity,
    increments: u32,
    last: Instant,
}

/// One rotation, resolved to the direction key its binding is stored under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RotaryTurn {
    /// The direction's binding key.
    pub(super) button: ButtonId,
    magnitude: u32,
    per_revolution: u16,
}

impl RotaryTurn {
    /// Split a signed increment count into its direction; zero is no turn.
    pub(super) fn new(rotary: Rotary, increments: i16, per_revolution: u16) -> Option<Self> {
        let button = match increments.signum() {
            1 => rotary.positive(),
            -1 => rotary.negative(),
            _ => return None,
        };
        Some(Self {
            button,
            magnitude: u32::from(increments.unsigned_abs()),
            per_revolution,
        })
    }

    /// Increments per revolution, treating an unknown resolution as one
    /// step per increment.
    fn resolution(self) -> f64 {
        if self.per_revolution == 0 {
            STEPS_PER_REVOLUTION
        } else {
            f64::from(self.per_revolution)
        }
    }
}

impl RotaryAccumulators {
    /// Advance `turn` under its direction's `action`.
    pub(super) fn advance(
        &mut self,
        turn: RotaryTurn,
        action: &Action,
        sensitivity: ThumbwheelSensitivity,
        now: Instant,
    ) -> RotaryOutput {
        if matches!(action, Action::None) {
            self.directions.remove(&turn.button);
            return RotaryOutput::Idle;
        }
        let multiplier = sensitivity.scroll_multiplier();
        if let Some(delta) = scroll(action, turn, multiplier) {
            self.directions.remove(&turn.button);
            return RotaryOutput::Scroll(delta);
        }
        let progress = self
            .directions
            .entry(turn.button)
            .or_insert_with(|| Progress {
                action: action.clone(),
                sensitivity,
                increments: 0,
                last: now,
            });
        if progress.action != *action
            || progress.sensitivity != sensitivity
            || now.duration_since(progress.last) > STEP_DECAY
        {
            progress.action = action.clone();
            progress.sensitivity = sensitivity;
            progress.increments = 0;
        }
        progress.last = now;
        progress.increments = progress.increments.saturating_add(turn.magnitude);
        let step = increments_per_step(turn, multiplier);
        let fires = progress.increments / step;
        progress.increments %= step;
        match fires.min(MAX_FIRES_PER_EVENT) {
            0 => RotaryOutput::Idle,
            n => RotaryOutput::Fire(n),
        }
    }
}

/// Increments that make one discrete step: the revolution split into
/// [`STEPS_PER_REVOLUTION`], scaled by the sensitivity, never below one.
fn increments_per_step(turn: RotaryTurn, multiplier: f64) -> u32 {
    let step = (turn.resolution() / (STEPS_PER_REVOLUTION * multiplier)).round();
    if step.is_finite() && step >= 1.0 {
        // Bounded by the u16 resolution, so the cast cannot truncate.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "step is finite, >= 1 and at most a u16 resolution"
        )]
        let step = step as u32;
        step
    } else {
        1
    }
}

/// The scroll a continuous-scroll binding produces for `turn`, or `None` for
/// any other action.
fn scroll(action: &Action, turn: RotaryTurn, multiplier: f64) -> Option<ScrollDelta> {
    let distance =
        f64::from(turn.magnitude) * STEPS_PER_REVOLUTION / turn.resolution() * multiplier;
    Some(match action {
        Action::ScrollUp => ScrollDelta::wheel_ticks(0.0, distance),
        Action::ScrollDown => ScrollDelta::wheel_ticks(0.0, -distance),
        Action::HorizontalScrollRight => ScrollDelta::wheel_ticks(distance, 0.0),
        Action::HorizontalScrollLeft => ScrollDelta::wheel_ticks(-distance, 0.0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIAL: u16 = 180;
    const ROLLER: u16 = 40;

    fn turn(rotary: Rotary, increments: i16, per_revolution: u16) -> RotaryTurn {
        RotaryTurn::new(rotary, increments, per_revolution).unwrap()
    }

    #[test]
    fn direction_follows_the_sign() {
        assert_eq!(turn(Rotary::Dial, 3, DIAL).button, ButtonId::DialRight);
        assert_eq!(turn(Rotary::Dial, -1, DIAL).button, ButtonId::DialLeft);
        assert_eq!(turn(Rotary::Roller, 1, ROLLER).button, ButtonId::RollerUp);
        assert!(RotaryTurn::new(Rotary::Roller, 0, ROLLER).is_none());
    }

    #[test]
    fn every_dial_step_fires_without_throttling() {
        let mut acc = RotaryAccumulators::default();
        let now = Instant::now();
        let action = Action::VolumeUp;
        // 180 increments per turn / 24 steps → one step every 8 (7.5 rounded).
        let mut fired = 0;
        for i in 0..80u64 {
            if let RotaryOutput::Fire(n) = acc.advance(
                turn(Rotary::Dial, 1, DIAL),
                &action,
                ThumbwheelSensitivity::DEFAULT,
                now + Duration::from_millis(i * 5),
            ) {
                fired += n;
            }
        }
        assert_eq!(fired, 10);
    }

    #[test]
    fn a_fast_burst_fires_several_steps_at_once() {
        let mut acc = RotaryAccumulators::default();
        let out = acc.advance(
            turn(Rotary::Roller, 4, ROLLER),
            &Action::Copy,
            ThumbwheelSensitivity::DEFAULT,
            Instant::now(),
        );
        // 40 / 24 rounds to a step of 2 increments.
        assert_eq!(out, RotaryOutput::Fire(2));
    }

    #[test]
    fn partial_progress_decays_when_idle() {
        let mut acc = RotaryAccumulators::default();
        let start = Instant::now();
        let action = Action::Paste;
        let sens = ThumbwheelSensitivity::DEFAULT;
        assert_eq!(
            acc.advance(turn(Rotary::Roller, 1, ROLLER), &action, sens, start),
            RotaryOutput::Idle
        );
        assert_eq!(
            acc.advance(
                turn(Rotary::Roller, 1, ROLLER),
                &action,
                sens,
                start + STEP_DECAY * 2
            ),
            RotaryOutput::Idle
        );
    }

    #[test]
    fn scroll_bindings_scroll_in_proportion() {
        let mut acc = RotaryAccumulators::default();
        let RotaryOutput::Scroll(delta) = acc.advance(
            turn(Rotary::Dial, -90, DIAL),
            &Action::ScrollDown,
            ThumbwheelSensitivity::DEFAULT,
            Instant::now(),
        ) else {
            panic!("expected scroll");
        };
        // Half a revolution scrolls half of STEPS_PER_REVOLUTION notches down.
        assert!((delta.y() + 12.0).abs() < 1e-9, "{delta:?}");
    }

    #[test]
    fn unbound_directions_stay_idle() {
        let mut acc = RotaryAccumulators::default();
        assert_eq!(
            acc.advance(
                turn(Rotary::Dial, 30, DIAL),
                &Action::None,
                ThumbwheelSensitivity::DEFAULT,
                Instant::now(),
            ),
            RotaryOutput::Idle
        );
    }
}

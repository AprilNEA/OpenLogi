//! Turning wheel travel over a scrollable slot into whole steps.
//!
//! A step is one notch of a ratcheting wheel. Hosts deliver a notch in
//! different shapes — several lines at once, or a high-resolution wheel's
//! fractions of a line, or a touchpad's pixels — so the travel is accumulated
//! and only whole steps leave this module.

use std::num::NonZeroU8;

use gpui::ScrollDelta;
use openlogi_core::binding::{ActionRingSlot, RingScrollDirection};

/// Lines one wheel notch scrolls, as GPUI reports it. Linux uses a fixed
/// three, and Windows its "lines per notch" setting, whose default is three.
/// AppKit reports one line per notch.
#[cfg(not(target_os = "macos"))]
const LINES_PER_STEP: f32 = 3.0;
#[cfg(target_os = "macos")]
const LINES_PER_STEP: f32 = 1.0;

/// Pixels of precise scrolling (a touchpad, a free-spinning wheel reported
/// in pixels) that make one step: about one notch's worth of document.
const PIXELS_PER_STEP: f32 = 48.0;

/// Wheel travel accumulated over one slot.
#[derive(Debug, Default)]
pub(crate) struct WheelSteps {
    /// The slot the travel was accumulated over.
    slot: Option<ActionRingSlot>,
    /// Steps of travel not yet reported, always less than one either way.
    /// Positive is up.
    travel: f32,
}

impl WheelSteps {
    /// Add `delta` of travel over `slot` and return the whole steps it
    /// completes, if any.
    ///
    /// Moving to another slot or reversing the wheel starts from zero, so
    /// leftover travel never turns into a step the hand did not make.
    pub(crate) fn turn(
        &mut self,
        slot: ActionRingSlot,
        delta: ScrollDelta,
    ) -> Option<(RingScrollDirection, NonZeroU8)> {
        let turned = match delta {
            ScrollDelta::Lines(lines) => lines.y / LINES_PER_STEP,
            ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / PIXELS_PER_STEP,
        };
        if !turned.is_finite() || turned == 0.0 {
            return None;
        }
        if self.slot != Some(slot) || self.travel * turned < 0.0 {
            self.slot = Some(slot);
            self.travel = 0.0;
        }
        self.travel += turned;
        let whole = self.travel.trunc();
        if whole == 0.0 {
            return None;
        }
        self.travel -= whole;
        let direction = if whole > 0.0 {
            RingScrollDirection::Up
        } else {
            RingScrollDirection::Down
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-zero whole count clamped to 1..=255 fits a u8"
        )]
        let steps = whole.abs().clamp(1.0, f32::from(u8::MAX)) as u8;
        NonZeroU8::new(steps).map(|steps| (direction, steps))
    }

    /// Forget accumulated travel, as when the pointer leaves every slot.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    fn lines(y: f32) -> ScrollDelta {
        ScrollDelta::Lines(point(0.0, y))
    }

    fn steps(n: u8) -> NonZeroU8 {
        NonZeroU8::new(n).expect("non-zero")
    }

    #[test]
    fn one_notch_is_one_step() {
        let mut wheel = WheelSteps::default();
        assert_eq!(
            wheel.turn(ActionRingSlot::Bottom, lines(LINES_PER_STEP)),
            Some((RingScrollDirection::Up, steps(1)))
        );
        assert_eq!(
            wheel.turn(ActionRingSlot::Bottom, lines(-LINES_PER_STEP)),
            Some((RingScrollDirection::Down, steps(1)))
        );
    }

    #[test]
    fn a_high_resolution_notch_arrives_in_fractions_and_steps_once() {
        let mut wheel = WheelSteps::default();
        // value120 = 15: an eighth of a notch per event.
        let eighth = lines(LINES_PER_STEP / 8.0);
        let fired: Vec<_> = (0..8)
            .filter_map(|_| wheel.turn(ActionRingSlot::Bottom, eighth))
            .collect();
        assert_eq!(fired, [(RingScrollDirection::Up, steps(1))]);
    }

    #[test]
    fn several_notches_in_one_frame_are_all_counted() {
        let mut wheel = WheelSteps::default();
        assert_eq!(
            wheel.turn(ActionRingSlot::Bottom, lines(-3.0 * LINES_PER_STEP)),
            Some((RingScrollDirection::Down, steps(3)))
        );
    }

    #[test]
    fn precise_pixels_step_by_distance() {
        let mut wheel = WheelSteps::default();
        let half = ScrollDelta::Pixels(point(px(0.0), px(PIXELS_PER_STEP / 2.0)));
        assert_eq!(wheel.turn(ActionRingSlot::Top, half), None);
        assert_eq!(
            wheel.turn(ActionRingSlot::Top, half),
            Some((RingScrollDirection::Up, steps(1)))
        );
    }

    #[test]
    fn leftover_travel_does_not_follow_the_pointer_or_a_reversal() {
        let mut wheel = WheelSteps::default();
        let most = lines(LINES_PER_STEP * 0.9);
        let little = lines(LINES_PER_STEP * 0.2);
        assert_eq!(wheel.turn(ActionRingSlot::Bottom, most), None);
        assert_eq!(
            wheel.turn(ActionRingSlot::Top, little),
            None,
            "another slot starts over"
        );

        assert_eq!(
            wheel.turn(ActionRingSlot::Top, most),
            Some((RingScrollDirection::Up, steps(1)))
        );
        let back = lines(-LINES_PER_STEP * 0.9);
        assert_eq!(
            wheel.turn(ActionRingSlot::Top, back),
            None,
            "a reversal starts over"
        );

        wheel.reset();
        assert_eq!(wheel.turn(ActionRingSlot::Top, little), None);
    }

    #[test]
    fn horizontal_and_empty_travel_is_ignored() {
        let mut wheel = WheelSteps::default();
        let sideways = ScrollDelta::Lines(point(LINES_PER_STEP * 4.0, 0.0));
        assert_eq!(wheel.turn(ActionRingSlot::Bottom, sideways), None);
        assert_eq!(wheel.turn(ActionRingSlot::Bottom, lines(f32::NAN)), None);
    }

    #[test]
    fn an_enormous_frame_is_capped_at_one_report() {
        let mut wheel = WheelSteps::default();
        assert_eq!(
            wheel.turn(ActionRingSlot::Bottom, lines(LINES_PER_STEP * 1000.0)),
            Some((RingScrollDirection::Up, steps(u8::MAX)))
        );
    }
}

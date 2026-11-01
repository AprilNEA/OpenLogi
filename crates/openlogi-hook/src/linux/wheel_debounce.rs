//! libinput's scroll-wheel debouncing, re-applied in front of the
//! pass-through device.
//!
//! libinput holds back small hi-res wheel movements from rest until they add
//! up to more than [`START_THRESHOLD`], so a wheel that rocks under a middle
//! click scrolls nothing (its `ModelScrollOnMiddleClick` quirk covers the MX
//! Master family). It skips that filter on virtual devices, i.e. any node
//! under `/sys/devices/virtual/input/`, and the pass-through mouse is a uinput
//! device there. Without this filter every rock reached the desktop as a
//! scroll as soon as the mouse was grabbed (#2241).
//!
//! The rules are those of libinput's mouse-wheel plugin in its
//! always-accumulate mode:
//! - From rest, movement accumulates until one axis has moved more than the
//!   threshold. That axis then delivers everything it moved at once, and the
//!   wheel is scrolling.
//! - While scrolling, each movement is delivered as it comes.
//! - A change of direction (the other sign or the other axis), or
//!   [`SCROLL_TIMEOUT`] without movement, puts the wheel back at rest. Held
//!   movement is kept, so a rock back and forth cancels out.
//!
//! The hook cannot read libinput's quirk database, so every grabbed hi-res
//! wheel gets the always-accumulate mode the MX Master family gets natively.
//! A held movement is not dispatched, so its low-res companion is dropped
//! with it.

use std::time::{Duration, Instant};

use evdev::{EventSummary, EventType, InputEvent, RelativeAxisCode};

/// libinput's `ACC_V120_THRESHOLD`: how far a wheel at rest must move, in
/// 1/120 of a notch, before it scrolls.
const START_THRESHOLD: u32 = 47;
/// libinput's `WHEEL_SCROLL_TIMEOUT`: how long a scrolling wheel may stay
/// still before it is at rest again.
const SCROLL_TIMEOUT: Duration = Duration::from_millis(500);

/// A hi-res wheel axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelAxis {
    Vertical,
    Horizontal,
}

impl TryFrom<RelativeAxisCode> for WheelAxis {
    type Error = ();

    fn try_from(code: RelativeAxisCode) -> Result<Self, Self::Error> {
        match code {
            RelativeAxisCode::REL_WHEEL_HI_RES => Ok(Self::Vertical),
            RelativeAxisCode::REL_HWHEEL_HI_RES => Ok(Self::Horizontal),
            _ => Err(()),
        }
    }
}

/// The way the wheel last moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Direction {
    axis: WheelAxis,
    positive: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// Movement accumulates until it passes [`START_THRESHOLD`].
    #[default]
    Resting,
    /// Movement is delivered as it comes. `last` is the latest delivery.
    Scrolling { last: Instant },
}

/// Movement accumulated while the wheel is at rest, per axis.
#[derive(Debug, Default)]
struct Held {
    vertical: i32,
    horizontal: i32,
}

impl Held {
    fn on(&mut self, axis: WheelAxis) -> &mut i32 {
        match axis {
            WheelAxis::Vertical => &mut self.vertical,
            WheelAxis::Horizontal => &mut self.horizontal,
        }
    }
}

/// The wheel-debounce state of one grabbed mouse.
#[derive(Debug, Default)]
pub(super) struct WheelDebounce {
    phase: Phase,
    direction: Option<Direction>,
    held: Held,
}

impl WheelDebounce {
    /// Debounce `event`, read at `now`, if it is a hi-res wheel movement.
    /// `None` holds it back. Any other event passes unchanged.
    pub(super) fn filter(&mut self, event: InputEvent, now: Instant) -> Option<InputEvent> {
        let EventSummary::RelativeAxis(_, code, value) = event.destructure() else {
            return Some(event);
        };
        let Ok(axis) = WheelAxis::try_from(code) else {
            return Some(event);
        };
        self.movement(axis, value, now)
            .map(|value| InputEvent::new(EventType::RELATIVE.0, code.0, value))
    }

    /// Take a movement of `value` on `axis` at `now`. Returns the movement to
    /// deliver, which includes what was held on that axis, or `None` while
    /// the wheel stays at rest. A zero movement carries no direction and
    /// changes nothing.
    fn movement(&mut self, axis: WheelAxis, value: i32, now: Instant) -> Option<i32> {
        if value == 0 {
            return None;
        }
        let direction = Direction {
            axis,
            positive: value > 0,
        };
        if self.direction.replace(direction) != Some(direction) || self.timed_out(now) {
            self.phase = Phase::Resting;
        }
        let held = self.held.on(axis);
        *held = held.saturating_add(value);
        if self.phase == Phase::Resting && held.unsigned_abs() <= START_THRESHOLD {
            return None;
        }
        self.phase = Phase::Scrolling { last: now };
        Some(std::mem::take(held))
    }

    fn timed_out(&self, now: Instant) -> bool {
        matches!(
            self.phase,
            Phase::Scrolling { last } if now.saturating_duration_since(last) >= SCROLL_TIMEOUT
        )
    }
}

#[cfg(test)]
mod tests {
    use evdev::KeyCode;

    use super::*;
    use WheelAxis::{Horizontal, Vertical};

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn rel(code: RelativeAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, code.0, value)
    }

    fn parts(event: InputEvent) -> (u16, u16, i32) {
        (event.event_type().0, event.code(), event.value())
    }

    /// The two middle clicks of #2241 as the MX Master 2S reported them: the
    /// wheel rocks 1/8 of a notch up as it is pressed and back while it is
    /// held or as it is released. libinput scrolls neither click.
    #[test]
    fn a_middle_click_rock_scrolls_nothing() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 0)), None);
        assert_eq!(wheel.movement(Vertical, -15, ms(start, 113)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 11_363)), None);
        assert_eq!(wheel.movement(Vertical, -15, ms(start, 11_453)), None);
    }

    /// A turn from rest scrolls once it has moved past the threshold, by all
    /// it has moved, and then follows every movement.
    #[test]
    fn a_turn_from_rest_scrolls_by_all_it_moved_past_the_threshold() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        let delivered: Vec<_> = (0..6)
            .map(|i| wheel.movement(Vertical, 15, ms(start, 8 * i)))
            .collect();
        assert_eq!(delivered, [None, None, None, Some(60), Some(15), Some(15)]);
    }

    /// libinput starts scrolling only above the threshold, not at it.
    #[test]
    fn the_threshold_itself_stays_at_rest() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 47, ms(start, 0)), None);
        assert_eq!(wheel.movement(Vertical, 1, ms(start, 8)), Some(48));
    }

    /// A detented wheel moves a whole notch per report, which passes at once.
    #[test]
    fn a_whole_notch_scrolls_at_once() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, -120, ms(start, 0)), Some(-120));
        assert_eq!(wheel.movement(Horizontal, 120, ms(start, 8)), Some(120));
    }

    /// The reverse half of a rock is accumulated, not dropped, so a later
    /// turn is neither short nor long by it.
    #[test]
    fn held_movement_survives_a_reversal() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 30, ms(start, 0)), None);
        assert_eq!(wheel.movement(Vertical, -15, ms(start, 50)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 100)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 108)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 116)), Some(60));
    }

    /// Reversing a scrolling wheel puts it back at rest, so a small step back
    /// is held like any movement from rest.
    #[test]
    fn a_reversal_while_scrolling_starts_from_rest() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 120, ms(start, 0)), Some(120));
        for t in [8, 16, 24] {
            assert_eq!(wheel.movement(Vertical, -15, ms(start, t)), None);
        }
        assert_eq!(wheel.movement(Vertical, -15, ms(start, 32)), Some(-60));
    }

    /// A scrolling wheel that stays still for the timeout is at rest again.
    /// One that moves sooner keeps scrolling.
    #[test]
    fn a_pause_of_the_timeout_ends_the_scroll() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 120, ms(start, 0)), Some(120));
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 499)), Some(15));
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 999)), None);
    }

    /// Each axis is a direction of its own. Switching axis is a change of
    /// direction, and each axis keeps what it holds.
    #[test]
    fn switching_axis_is_a_change_of_direction() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 120, ms(start, 0)), Some(120));
        assert_eq!(wheel.movement(Horizontal, 15, ms(start, 8)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 16)), None);
        assert_eq!(wheel.movement(Horizontal, 120, ms(start, 24)), Some(135));
        assert_eq!(wheel.movement(Vertical, 45, ms(start, 32)), Some(60));
    }

    #[test]
    fn a_zero_movement_changes_nothing() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        assert_eq!(wheel.movement(Vertical, 120, ms(start, 0)), Some(120));
        assert_eq!(wheel.movement(Vertical, 0, ms(start, 8)), None);
        assert_eq!(wheel.movement(Vertical, 15, ms(start, 16)), Some(15));
    }

    /// Only hi-res wheel axes are debounced. Low-res wheel events follow
    /// their hi-res companion elsewhere; pointer motion and buttons are not
    /// wheel movement.
    #[test]
    fn filter_debounces_only_hi_res_wheel_events() {
        let start = Instant::now();
        let mut wheel = WheelDebounce::default();
        for event in [
            rel(RelativeAxisCode::REL_X, 3),
            rel(RelativeAxisCode::REL_WHEEL, 1),
            rel(RelativeAxisCode::REL_HWHEEL, -1),
            InputEvent::new(EventType::KEY.0, KeyCode::BTN_MIDDLE.0, 1),
        ] {
            let passed = wheel.filter(event, start).map(parts);
            assert_eq!(passed, Some(parts(event)), "{event:?} must pass unchanged");
        }
        assert_eq!(
            wheel
                .filter(rel(RelativeAxisCode::REL_WHEEL_HI_RES, 15), start)
                .map(parts),
            None
        );
        assert_eq!(
            wheel
                .filter(rel(RelativeAxisCode::REL_WHEEL_HI_RES, 45), start)
                .map(parts),
            Some(parts(rel(RelativeAxisCode::REL_WHEEL_HI_RES, 60)))
        );
    }
}

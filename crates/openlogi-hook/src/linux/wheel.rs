//! The wheel of a grabbed hi-res mouse as the pass-through device reports
//! it: libinput's scroll-wheel debouncing, and the low-res count that goes
//! out with each hi-res movement.
//!
//! # Debouncing
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
//! Time is the kernel's event time, so reports that queue up behind a slow
//! reader keep their gaps.
//!
//! # Low-res counts
//!
//! A hi-res device reports a low-res count (`REL_WHEEL`/`REL_HWHEEL`) next to
//! its hi-res movement. libinput ignores it when the hi-res one is present,
//! but desktops that read the *discrete* axis value (mutter 42) derive it from
//! the low-res count, so a pass-through that drops it scrolls nothing. A
//! report's low-res count is therefore forwarded exactly when the hi-res
//! movement on the same axis was. When the callback took the hi-res movement,
//! the action injector re-emits both, and a forwarded count would double the
//! scroll distance. While the wheel is at rest, the count is held with the
//! hi-res movement and goes out with it, as libinput holds both.

use std::time::{Duration, SystemTime};

use evdev::{EventSummary, EventType, InputEvent, RelativeAxisCode};

use super::invert_wheel;

/// libinput's `ACC_V120_THRESHOLD`: how far a wheel at rest must move, in
/// 1/120 of a notch, before it scrolls.
const START_THRESHOLD: u32 = 47;
/// libinput's `WHEEL_SCROLL_TIMEOUT`: how long a scrolling wheel may stay
/// still before it is at rest again.
const SCROLL_TIMEOUT: Duration = Duration::from_millis(500);

/// A wheel axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WheelAxis {
    Vertical,
    Horizontal,
}

impl WheelAxis {
    fn hi_res_code(self) -> RelativeAxisCode {
        match self {
            Self::Vertical => RelativeAxisCode::REL_WHEEL_HI_RES,
            Self::Horizontal => RelativeAxisCode::REL_HWHEEL_HI_RES,
        }
    }

    fn low_res_code(self) -> RelativeAxisCode {
        match self {
            Self::Vertical => RelativeAxisCode::REL_WHEEL,
            Self::Horizontal => RelativeAxisCode::REL_HWHEEL,
        }
    }
}

/// A wheel event from a device with a hi-res wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WheelEvent {
    HiRes(WheelAxis, i32),
    LowRes(WheelAxis, i32),
}

impl TryFrom<InputEvent> for WheelEvent {
    type Error = ();

    fn try_from(event: InputEvent) -> Result<Self, Self::Error> {
        let EventSummary::RelativeAxis(_, code, value) = event.destructure() else {
            return Err(());
        };
        match code {
            RelativeAxisCode::REL_WHEEL_HI_RES => Ok(Self::HiRes(WheelAxis::Vertical, value)),
            RelativeAxisCode::REL_HWHEEL_HI_RES => Ok(Self::HiRes(WheelAxis::Horizontal, value)),
            RelativeAxisCode::REL_WHEEL => Ok(Self::LowRes(WheelAxis::Vertical, value)),
            RelativeAxisCode::REL_HWHEEL => Ok(Self::LowRes(WheelAxis::Horizontal, value)),
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
    Scrolling { last: SystemTime },
}

/// Wheel movement on one axis, in hi-res units and low-res counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Movement {
    hi_res: i32,
    low_res: i32,
}

/// What became of the current report's hi-res movement on one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HiRes {
    /// None arrived, or the callback took the one that was delivered.
    #[default]
    NotForwarded,
    /// Held back while the wheel is at rest.
    Held,
    /// Passed through to the desktop.
    Forwarded,
}

#[derive(Debug, Default)]
struct Axis {
    /// Movement held while the wheel is at rest.
    held: Movement,
    /// The current report's low-res count, plus the held count that its
    /// hi-res movement released.
    low_res: i32,
    hi_res: HiRes,
}

/// The pass-through wheel state of one grabbed hi-res mouse.
#[derive(Debug, Default)]
pub(super) struct PassThroughWheel {
    phase: Phase,
    direction: Option<Direction>,
    vertical: Axis,
    horizontal: Axis,
}

impl PassThroughWheel {
    /// Take a hi-res movement of `value` on `axis`, reported at `at`. Returns
    /// the hi-res event to dispatch, which includes what was held on that
    /// axis, or `None` while the wheel stays at rest. A zero movement carries
    /// no direction and changes nothing.
    pub(super) fn hi_res(
        &mut self,
        axis: WheelAxis,
        value: i32,
        at: SystemTime,
    ) -> Option<InputEvent> {
        if value == 0 {
            return None;
        }
        let direction = Direction {
            axis,
            positive: value > 0,
        };
        if self.direction.replace(direction) != Some(direction) || self.timed_out(at) {
            self.phase = Phase::Resting;
        }
        let resting = self.phase == Phase::Resting;
        let state = self.axis(axis);
        state.held.hi_res = state.held.hi_res.saturating_add(value);
        if resting && state.held.hi_res.unsigned_abs() <= START_THRESHOLD {
            state.hi_res = HiRes::Held;
            return None;
        }
        let released = std::mem::take(&mut state.held);
        state.low_res = state.low_res.saturating_add(released.low_res);
        state.hi_res = HiRes::NotForwarded;
        self.phase = Phase::Scrolling { last: at };
        Some(InputEvent::new(
            EventType::RELATIVE.0,
            axis.hi_res_code().0,
            released.hi_res,
        ))
    }

    /// Take a low-res count. It goes out at the end of the report, with the
    /// hi-res movement on the same axis.
    pub(super) fn low_res(&mut self, axis: WheelAxis, value: i32) {
        let state = self.axis(axis);
        state.low_res = state.low_res.saturating_add(value);
    }

    /// Record that the event [`Self::hi_res`] returned for `axis` reached the
    /// desktop.
    pub(super) fn forwarded(&mut self, axis: WheelAxis) {
        self.axis(axis).hi_res = HiRes::Forwarded;
    }

    /// End the report: push the low-res events that go out with it into
    /// `pending`. `inverted` applies software scroll inversion to the
    /// vertical count too, so it agrees with its hi-res event for clients
    /// that read the discrete axis.
    pub(super) fn end_report(&mut self, pending: &mut Vec<InputEvent>, inverted: bool) {
        for axis in [WheelAxis::Vertical, WheelAxis::Horizontal] {
            let state = self.axis(axis);
            let low_res = std::mem::take(&mut state.low_res);
            match std::mem::take(&mut state.hi_res) {
                HiRes::Held => state.held.low_res = state.held.low_res.saturating_add(low_res),
                HiRes::Forwarded if low_res != 0 => pending.push(invert_wheel(
                    InputEvent::new(EventType::RELATIVE.0, axis.low_res_code().0, low_res),
                    inverted,
                )),
                HiRes::Forwarded | HiRes::NotForwarded => {}
            }
        }
    }

    fn axis(&mut self, axis: WheelAxis) -> &mut Axis {
        match axis {
            WheelAxis::Vertical => &mut self.vertical,
            WheelAxis::Horizontal => &mut self.horizontal,
        }
    }

    fn timed_out(&self, at: SystemTime) -> bool {
        // A clock that steps back reads as no time passed.
        matches!(
            self.phase,
            Phase::Scrolling { last }
                if at.duration_since(last).is_ok_and(|gap| gap >= SCROLL_TIMEOUT)
        )
    }
}

#[cfg(test)]
mod tests {
    use evdev::KeyCode;

    use super::*;
    use WheelAxis::{Horizontal, Vertical};

    const WHEEL: u16 = RelativeAxisCode::REL_WHEEL.0;

    /// The kernel time of the first #2241 report.
    fn start() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_793_000_560_097)
    }

    fn ms(ms: u64) -> SystemTime {
        start() + Duration::from_millis(ms)
    }

    /// The hi-res value [`PassThroughWheel::hi_res`] delivers, if any.
    fn hi(wheel: &mut PassThroughWheel, axis: WheelAxis, value: i32, at: u64) -> Option<i32> {
        wheel.hi_res(axis, value, ms(at)).map(|event| event.value())
    }

    /// One report on the vertical axis: a hi-res movement, with a low-res
    /// count when `low_res` is not zero. `forward` is the callback's answer.
    /// Returns the delivered hi-res value and the low-res events the report
    /// sends, as `(code, value)`.
    fn report(
        wheel: &mut PassThroughWheel,
        hi_res: i32,
        low_res: i32,
        at: u64,
        forward: bool,
    ) -> (Option<i32>, Vec<(u16, i32)>) {
        if low_res != 0 {
            wheel.low_res(Vertical, low_res);
        }
        let delivered = hi(wheel, Vertical, hi_res, at);
        if delivered.is_some() && forward {
            wheel.forwarded(Vertical);
        }
        let mut pending = Vec::new();
        wheel.end_report(&mut pending, false);
        let sent = pending.iter().map(|e| (e.code(), e.value())).collect();
        (delivered, sent)
    }

    // Debouncing

    /// The two middle clicks of #2241 as the MX Master 2S reported them: the
    /// wheel rocks 1/8 of a notch up as it is pressed and back while it is
    /// held or as it is released. libinput scrolls neither click.
    #[test]
    fn a_middle_click_rock_scrolls_nothing() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 15, 0), None);
        assert_eq!(hi(&mut wheel, Vertical, -15, 113), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 11_363), None);
        assert_eq!(hi(&mut wheel, Vertical, -15, 11_453), None);
    }

    /// A turn from rest scrolls once it has moved past the threshold, by all
    /// it has moved, and then follows every movement.
    #[test]
    fn a_turn_from_rest_scrolls_by_all_it_moved_past_the_threshold() {
        let mut wheel = PassThroughWheel::default();
        let delivered: Vec<_> = (0..6)
            .map(|i| hi(&mut wheel, Vertical, 15, 8 * i))
            .collect();
        assert_eq!(delivered, [None, None, None, Some(60), Some(15), Some(15)]);
    }

    /// libinput starts scrolling only above the threshold, not at it.
    #[test]
    fn the_threshold_itself_stays_at_rest() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 47, 0), None);
        assert_eq!(hi(&mut wheel, Vertical, 1, 8), Some(48));
    }

    /// A detented wheel moves a whole notch per report, which passes at once.
    #[test]
    fn a_whole_notch_scrolls_at_once() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, -120, 0), Some(-120));
        assert_eq!(hi(&mut wheel, Horizontal, 120, 8), Some(120));
    }

    /// The delivered event is the hi-res event of the axis that moved.
    #[test]
    fn the_delivered_event_names_its_hi_res_axis() {
        let mut wheel = PassThroughWheel::default();
        for (axis, code) in [
            (Vertical, RelativeAxisCode::REL_WHEEL_HI_RES),
            (Horizontal, RelativeAxisCode::REL_HWHEEL_HI_RES),
        ] {
            let event = wheel.hi_res(axis, 120, start()).expect("a notch passes");
            assert_eq!(event.event_type(), EventType::RELATIVE);
            assert_eq!(event.code(), code.0);
        }
    }

    /// The reverse half of a rock is accumulated, not dropped, so a later
    /// turn is neither short nor long by it.
    #[test]
    fn held_movement_survives_a_reversal() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 30, 0), None);
        assert_eq!(hi(&mut wheel, Vertical, -15, 50), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 100), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 108), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 116), Some(60));
    }

    /// Reversing a scrolling wheel puts it back at rest, so a small step back
    /// is held like any movement from rest.
    #[test]
    fn a_reversal_while_scrolling_starts_from_rest() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 120, 0), Some(120));
        for t in [8, 16, 24] {
            assert_eq!(hi(&mut wheel, Vertical, -15, t), None);
        }
        assert_eq!(hi(&mut wheel, Vertical, -15, 32), Some(-60));
    }

    /// A scrolling wheel that stays still for the timeout is at rest again.
    /// One that moves sooner keeps scrolling. The times are the reports'
    /// own, so a backlog read at one instant keeps its pause.
    #[test]
    fn a_pause_of_the_timeout_ends_the_scroll() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 120, 0), Some(120));
        assert_eq!(hi(&mut wheel, Vertical, 15, 499), Some(15));
        assert_eq!(hi(&mut wheel, Vertical, 15, 999), None);
    }

    #[test]
    fn a_clock_that_steps_back_keeps_the_scroll() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 120, 1_000), Some(120));
        assert_eq!(hi(&mut wheel, Vertical, 15, 0), Some(15));
    }

    /// Each axis is a direction of its own. Switching axis is a change of
    /// direction, and each axis keeps what it holds.
    #[test]
    fn switching_axis_is_a_change_of_direction() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 120, 0), Some(120));
        assert_eq!(hi(&mut wheel, Horizontal, 15, 8), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 16), None);
        assert_eq!(hi(&mut wheel, Horizontal, 120, 24), Some(135));
        assert_eq!(hi(&mut wheel, Vertical, 45, 32), Some(60));
    }

    #[test]
    fn a_zero_movement_changes_nothing() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(hi(&mut wheel, Vertical, 120, 0), Some(120));
        assert_eq!(hi(&mut wheel, Vertical, 0, 8), None);
        assert_eq!(hi(&mut wheel, Vertical, 15, 16), Some(15));
    }

    // Low-res counts

    #[test]
    fn a_low_res_count_goes_out_with_its_forwarded_hi_res_movement() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(
            report(&mut wheel, -120, -1, 0, true),
            (Some(-120), vec![(WHEEL, -1)])
        );
    }

    /// The callback took the movement, so the action injector re-emits both.
    #[test]
    fn a_low_res_count_is_dropped_when_the_callback_takes_its_movement() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(report(&mut wheel, -120, -1, 0, false), (Some(-120), vec![]));
    }

    #[test]
    fn low_res_axes_are_matched_independently() {
        let mut wheel = PassThroughWheel::default();
        wheel.low_res(Vertical, 1);
        wheel.low_res(Horizontal, 1);
        assert!(wheel.hi_res(Horizontal, 120, start()).is_some());
        wheel.forwarded(Horizontal);
        let mut pending = Vec::new();
        wheel.end_report(&mut pending, false);
        let codes: Vec<_> = pending.iter().map(InputEvent::code).collect();
        assert_eq!(codes, [RelativeAxisCode::REL_HWHEEL.0]);
    }

    #[test]
    fn forwarding_does_not_leak_into_the_next_report() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(
            report(&mut wheel, 120, 1, 0, true),
            (Some(120), vec![(WHEEL, 1)])
        );
        wheel.low_res(Vertical, 1);
        let mut pending = Vec::new();
        wheel.end_report(&mut pending, false);
        assert!(pending.is_empty());
    }

    /// The kernel can send a low-res tick while the hi-res movement is held,
    /// for example when a turn resumes after a pause. The tick goes out when
    /// the movement does, so a desktop reading discrete ticks loses no notch.
    #[test]
    fn a_tick_held_with_its_movement_goes_out_with_it() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(report(&mut wheel, 105, 0, 0, true), (Some(105), vec![]));
        assert_eq!(report(&mut wheel, 15, 1, 500, true), (None, vec![]));
        assert_eq!(report(&mut wheel, 15, 0, 508, true), (None, vec![]));
        assert_eq!(report(&mut wheel, 15, 0, 516, true), (None, vec![]));
        assert_eq!(
            report(&mut wheel, 15, 0, 524, true),
            (Some(60), vec![(WHEEL, 1)])
        );
    }

    /// A held tick follows its movement into the action injector too.
    #[test]
    fn a_held_tick_is_dropped_when_the_callback_takes_its_movement() {
        let mut wheel = PassThroughWheel::default();
        assert_eq!(report(&mut wheel, 15, 1, 0, true), (None, vec![]));
        assert_eq!(report(&mut wheel, 45, 0, 8, false), (Some(60), vec![]));
        assert_eq!(report(&mut wheel, 15, 0, 16, true), (Some(15), vec![]));
    }

    /// A forwarded low-res count is negated with its hi-res event, so a
    /// client reading the discrete axis scrolls the same way as one reading
    /// the hi-res axis.
    #[test]
    fn inversion_negates_the_forwarded_low_res_count() {
        let mut wheel = PassThroughWheel::default();
        wheel.low_res(Vertical, 1);
        assert!(wheel.hi_res(Vertical, 120, start()).is_some());
        wheel.forwarded(Vertical);
        let mut pending = Vec::new();
        wheel.end_report(&mut pending, true);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].value(), -1);
    }

    // Classification

    #[test]
    fn wheel_events_are_classified_by_axis_and_resolution() {
        let rel = |code: RelativeAxisCode| InputEvent::new(EventType::RELATIVE.0, code.0, 7);
        let cases = [
            (
                RelativeAxisCode::REL_WHEEL_HI_RES,
                WheelEvent::HiRes(Vertical, 7),
            ),
            (
                RelativeAxisCode::REL_HWHEEL_HI_RES,
                WheelEvent::HiRes(Horizontal, 7),
            ),
            (RelativeAxisCode::REL_WHEEL, WheelEvent::LowRes(Vertical, 7)),
            (
                RelativeAxisCode::REL_HWHEEL,
                WheelEvent::LowRes(Horizontal, 7),
            ),
        ];
        for (code, expected) in cases {
            assert_eq!(WheelEvent::try_from(rel(code)), Ok(expected), "{code:?}");
        }
        assert_eq!(WheelEvent::try_from(rel(RelativeAxisCode::REL_X)), Err(()));
        let button = InputEvent::new(EventType::KEY.0, KeyCode::BTN_MIDDLE.0, 1);
        assert_eq!(WheelEvent::try_from(button), Err(()));
    }
}

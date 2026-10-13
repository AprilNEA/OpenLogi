//! Motion-driven volume adjustment after a held gesture's first committed swipe.

use std::collections::BTreeMap;

use super::{Action, GESTURE_SWIPE_THRESHOLD, GestureDirection};

#[derive(Debug)]
enum Axis {
    Horizontal,
    Vertical,
}

/// Additional volume steps during an already-committed gesture hold.
///
/// The first swipe remains owned by [`super::SwipeAccumulator`]. This state is
/// created only for a volume binding; ordinary gestures therefore stay one-shot.
/// It owns the two volume bindings for that hold, so neither a reversal nor a
/// later binding change can repeat a desktop, shortcut, or other unrelated action.
#[derive(Debug)]
pub struct VolumeGesture {
    axis: Axis,
    negative: Option<Action>,
    positive: Option<Action>,
    remainder: i32,
}

impl VolumeGesture {
    /// Begin continuation after `direction` has dispatched its first volume
    /// action. Clicks and non-volume bindings never enter continuous mode.
    #[must_use]
    pub fn begin(
        direction: GestureDirection,
        bindings: &BTreeMap<GestureDirection, Action>,
    ) -> Option<Self> {
        volume_action(bindings.get(&direction))?;
        let (axis, negative, positive) = match direction {
            GestureDirection::Up | GestureDirection::Down => {
                (Axis::Vertical, GestureDirection::Up, GestureDirection::Down)
            }
            GestureDirection::Left | GestureDirection::Right => (
                Axis::Horizontal,
                GestureDirection::Left,
                GestureDirection::Right,
            ),
            GestureDirection::Click => return None,
        };
        Some(Self {
            axis,
            negative: volume_action(bindings.get(&negative)),
            positive: volume_action(bindings.get(&positive)),
            remainder: 0,
        })
    }

    /// Convert new motion into bounded volume steps, without any timer or
    /// stationary repeat. Reversing direction discards the old fractional step,
    /// so the mouse need not return to its original position to reverse volume.
    /// At most 16 steps are emitted per sample; excess travel is discarded rather
    /// than queued for a later stationary sample.
    pub fn advance(&mut self, dx: i32, dy: i32) -> Option<(Action, u8)> {
        let delta = match self.axis {
            Axis::Horizontal => dx,
            Axis::Vertical => dy,
        };
        if delta == 0 {
            return None;
        }
        if self.remainder.signum() != delta.signum() {
            self.remainder = 0;
        }
        self.remainder = self.remainder.saturating_add(delta);
        let steps = self.remainder.unsigned_abs() / GESTURE_SWIPE_THRESHOLD.cast_unsigned();
        self.remainder %= GESTURE_SWIPE_THRESHOLD;
        let action = if delta < 0 {
            self.negative.as_ref()
        } else {
            self.positive.as_ref()
        }?;
        // Clamp before the conversion; the fallback cannot be reached.
        let steps = u8::try_from(steps.min(16)).unwrap_or(16);
        (steps != 0).then(|| (action.clone(), steps))
    }
}

fn volume_action(action: Option<&Action>) -> Option<Action> {
    action
        .filter(|action| matches!(action, Action::VolumeUp | Action::VolumeDown))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings() -> BTreeMap<GestureDirection, Action> {
        BTreeMap::from([
            (GestureDirection::Up, Action::VolumeUp),
            (GestureDirection::Down, Action::VolumeDown),
            (GestureDirection::Left, Action::None),
            (GestureDirection::Right, Action::None),
            (GestureDirection::Click, Action::PreviousDesktop),
        ])
    }

    #[test]
    fn motion_accumulates_steps_and_reverses_without_returning_to_origin() {
        let mut gesture = VolumeGesture::begin(GestureDirection::Up, &bindings()).unwrap();
        assert_eq!(gesture.advance(0, -24), None);
        assert_eq!(gesture.advance(0, -27), Some((Action::VolumeUp, 1)));
        assert_eq!(gesture.advance(0, -100), Some((Action::VolumeUp, 2)));
        assert_eq!(gesture.advance(0, 49), None);
        assert_eq!(gesture.advance(0, 1), Some((Action::VolumeDown, 1)));
        assert_eq!(gesture.advance(0, -50), Some((Action::VolumeUp, 1)));
    }

    #[test]
    fn stationary_or_cross_axis_motion_never_repeats() {
        let mut gesture = VolumeGesture::begin(GestureDirection::Up, &bindings()).unwrap();
        assert_eq!(gesture.advance(0, -49), None);
        assert_eq!(gesture.advance(1000, 0), None);
        assert_eq!(gesture.advance(-1000, 0), None);
        assert_eq!(gesture.advance(0, 0), None);
    }

    #[test]
    fn only_volume_actions_can_start_or_continue() {
        let mut map = bindings();
        for direction in [
            GestureDirection::Left,
            GestureDirection::Right,
            GestureDirection::Click,
        ] {
            assert!(VolumeGesture::begin(direction, &map).is_none());
        }
        map.insert(GestureDirection::Down, Action::NextDesktop);
        let mut gesture = VolumeGesture::begin(GestureDirection::Up, &map).unwrap();
        assert_eq!(gesture.advance(0, 100), None);
        assert_eq!(gesture.advance(0, -50), Some((Action::VolumeUp, 1)));
        assert!(VolumeGesture::begin(GestureDirection::Down, &map).is_none());
    }

    #[test]
    fn extreme_samples_are_bounded_and_leave_no_stationary_backlog() {
        let mut gesture = VolumeGesture::begin(GestureDirection::Down, &bindings()).unwrap();
        assert_eq!(gesture.advance(0, i32::MAX), Some((Action::VolumeDown, 16)));
        assert_eq!(gesture.advance(0, 0), None);
        assert_eq!(gesture.advance(0, i32::MIN), Some((Action::VolumeUp, 16)));
        assert_eq!(gesture.advance(0, 0), None);
    }
}

//! Thumb-wheel reports: rolls, taps, and contact without either.

use super::*;

/// The resolutions an MX Master 4 reports: 20 ratchets natively, 120
/// increments diverted, so one increment is a sixth of a native scroll unit.
const TRACED_RES: thumbwheel::WheelResolution = thumbwheel::WheelResolution {
    native_res: 20,
    diverted_res: 120,
};

fn thumb_event(
    rotation: i16,
    rotation_status: thumbwheel::RotationStatus,
    single_tap: bool,
) -> thumbwheel::ThumbwheelEvent {
    thumbwheel::ThumbwheelEvent {
        rotation,
        rotation_status,
        single_tap,
        touch: true,
        proxy: true,
    }
}

/// The wheel's touch sensor flags a tap on the same contact that rolled it, so
/// a rolling report's tap bit is an artifact — forwarding it fired the tap's
/// bound action in the middle of a scroll.
#[test]
fn a_rolling_report_is_a_roll_even_when_it_flags_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(-3, thumbwheel::RotationStatus::Active, true),
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: -3,
            resolution: TRACED_RES
        })
    );
}

/// The roll's closing report: the finger lifts, so it carries no rotation of
/// its own while the sensor flags the contact that just rolled the wheel. Only
/// `rotation_status` separates it from a deliberate tap.
#[test]
fn the_release_that_ends_a_roll_is_not_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Stop, true),
            TRACED_RES
        ),
        None
    );
}

#[test]
fn a_tap_on_a_settled_wheel_is_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Inactive, true),
            TRACED_RES
        ),
        Some(CapturedInput::ButtonPulse(ButtonId::Thumbwheel))
    );
}

/// A wheel whose firmware leaves byte 4 at zero still has its own rotation to
/// go on, so the roll is recognised without the status field.
#[test]
fn rotation_alone_still_marks_a_roll() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(4, thumbwheel::RotationStatus::Inactive, true),
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: 4,
            resolution: TRACED_RES
        })
    );
}

/// Touch and proximity alone carry no input: the wheel reports them whenever a
/// thumb rests near it.
#[test]
fn contact_without_rotation_or_a_tap_carries_no_input() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Inactive, false),
            TRACED_RES
        ),
        None
    );
}

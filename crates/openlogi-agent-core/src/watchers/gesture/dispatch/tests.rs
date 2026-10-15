use openlogi_hid::thumbwheel::WheelResolution;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::*;

fn rotation(magnitude: i32) -> WheelRotation {
    let increments = i16::try_from(magnitude).expect("test magnitude fits in i16");
    WheelRotation::from_increments(increments).expect("non-zero test rotation")
}

fn scale() -> ScrollScale {
    ScrollScale::new(WheelResolution::UNKNOWN, ThumbwheelSensitivity::DEFAULT)
}

#[test]
fn replacement_session_does_not_inherit_progress_or_cooldown() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    let now = Instant::now();
    let mut wheels = SessionWheels::default();

    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(threshold), &Action::NextTab, scale(), now,),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(threshold), &Action::NextTab, scale(), now,),
        WheelOutput::Idle,
        "the old session must still be cooling down"
    );
    assert_eq!(
        wheels.for_session(&replacement).advance(
            rotation(threshold),
            &Action::NextTab,
            scale(),
            now,
        ),
        WheelOutput::FireAction(1),
        "a new session must not inherit the old session's cooldown"
    );

    wheels.cancel_session(&old);
    assert!(
        wheels.0.contains_key(&replacement),
        "canceling a stale epoch must not erase its replacement's state"
    );
}

#[test]
fn replacement_session_does_not_inherit_partial_progress() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    let now = Instant::now();
    let mut wheels = SessionWheels::default();

    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(threshold - 1), &Action::VolumeUp, scale(), now,),
        WheelOutput::Idle
    );
    assert_eq!(
        wheels
            .for_session(&replacement)
            .advance(rotation(1), &Action::VolumeUp, scale(), now,),
        WheelOutput::Idle,
        "a new session must start with no action progress"
    );
}

fn per_notch(units: u8) -> NonZeroU8 {
    NonZeroU8::new(units).expect("non-zero test multiplier")
}

fn horizontal(delta: ScrollDelta) -> f64 {
    assert!(
        delta.y().abs() < f64::EPSILON,
        "a held wheel never scrolls vertically"
    );
    delta.x()
}

/// The thumb wheel's default bindings scroll left for its "up" direction, so
/// the virtual one does too: away from the user is left, toward is right.
#[test]
fn a_held_wheel_scrolls_left_away_from_the_user_and_right_toward() {
    assert!((horizontal(held_wheel_scroll(1, NonZeroU8::MIN, false)) + 1.0).abs() < f64::EPSILON);
    assert!((horizontal(held_wheel_scroll(-3, NonZeroU8::MIN, false)) - 3.0).abs() < f64::EPSILON);
}

#[test]
fn a_high_resolution_held_wheel_scrolls_its_share_of_a_notch() {
    assert!((horizontal(held_wheel_scroll(4, per_notch(8), false)) + 0.5).abs() < f64::EPSILON);
    assert!((horizontal(held_wheel_scroll(-16, per_notch(8), false)) - 2.0).abs() < f64::EPSILON);
}

#[test]
fn an_inverted_wheel_inverts_its_held_scroll() {
    assert!((horizontal(held_wheel_scroll(1, NonZeroU8::MIN, true)) - 1.0).abs() < f64::EPSILON);
    assert!((horizontal(held_wheel_scroll(-8, per_notch(8), true)) + 1.0).abs() < f64::EPSILON);
}

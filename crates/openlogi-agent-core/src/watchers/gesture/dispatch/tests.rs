use openlogi_hid::thumbwheel::WheelResolution;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::*;
use openlogi_core::binding::GestureDirection;
use std::collections::BTreeMap;

#[test]
fn volume_progress_belongs_to_one_press_and_capture_session() {
    let first = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let bindings = BTreeMap::from([
        (GestureDirection::Up, Action::VolumeUp),
        (GestureDirection::Down, Action::VolumeDown),
    ]);
    let mut presses = GesturePresses::default();
    presses.start(
        &first,
        ButtonId::Back,
        PressToken::hook_for_test(1, ButtonId::Back),
    );
    let press = presses.get_mut(&first, ButtonId::Back).unwrap();
    press.volume = VolumeGesture::begin(GestureDirection::Up, &bindings);
    assert_eq!(press.volume.as_mut().unwrap().advance(0, -49), None);

    presses.start(
        &replacement,
        ButtonId::Back,
        PressToken::hook_for_test(2, ButtonId::Back),
    );
    assert!(
        presses
            .get_mut(&replacement, ButtonId::Back)
            .unwrap()
            .volume
            .is_none()
    );
    presses.cancel_session(&first);
    assert!(presses.get_mut(&first, ButtonId::Back).is_none());
    assert!(presses.get_mut(&replacement, ButtonId::Back).is_some());

    presses
        .get_mut(&replacement, ButtonId::Back)
        .unwrap()
        .volume = VolumeGesture::begin(GestureDirection::Down, &bindings);
    presses.start(
        &replacement,
        ButtonId::Back,
        PressToken::hook_for_test(3, ButtonId::Back),
    );
    assert!(
        presses
            .get_mut(&replacement, ButtonId::Back)
            .unwrap()
            .volume
            .is_none(),
        "re-press discards the previous volume hold"
    );
    presses.end(&replacement, ButtonId::Back);
    assert!(presses.get_mut(&replacement, ButtonId::Back).is_none());
}

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

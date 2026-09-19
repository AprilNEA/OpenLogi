use std::collections::BTreeMap;

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
fn every_hidpp_raw_xy_gesture_source_is_an_interactive_space_candidate() {
    let plan = DispatchPlan {
        config_key: "mouse-a".to_owned(),
        bindings: BTreeMap::new(),
        gesture_bindings: BTreeMap::from([(ButtonId::GestureButton, BTreeMap::new())]),
        side_gesture_bindings: BTreeMap::from([(ButtonId::Forward, BTreeMap::new())]),
        thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
    };

    assert!(is_interactive_space_source(&plan, ButtonId::GestureButton));
    assert!(is_interactive_space_source(&plan, ButtonId::Forward));
    assert!(
        !is_interactive_space_source(&plan, ButtonId::MiddleClick),
        "OS-hook-only buttons must stay on the one-shot path"
    );
}

#[test]
fn interactive_space_transition_uses_the_bound_desktop_direction_and_tracks_reversal() {
    let session = HidppSessionId::with_epoch("mouse-a", 7);
    let mut transitions = InteractiveSpaceTransitions::default();

    assert!(transitions.begin(
        &session,
        ButtonId::GestureButton,
        GestureDirection::Left,
        &Action::NextDesktop,
    ));
    assert_eq!(
        transitions.motion(&session, ButtonId::GestureButton, -120),
        Some(SpaceSwipeFrame {
            progress_x: 120.0,
            phase: openlogi_inject::InteractiveSpacePhase::Began,
        }),
        "a custom left → next mapping must still begin in the next-Space direction"
    );
    assert_eq!(
        transitions.motion(&session, ButtonId::GestureButton, 20),
        Some(SpaceSwipeFrame {
            progress_x: 100.0,
            phase: openlogi_inject::InteractiveSpacePhase::Changed,
        }),
        "reversing the held gesture must retract transition progress"
    );
    assert_eq!(
        transitions.end(&session, ButtonId::GestureButton),
        Some(SpaceSwipeFrame {
            progress_x: 100.0,
            phase: openlogi_inject::InteractiveSpacePhase::Ended,
        })
    );
}

#[test]
fn interactive_space_transition_cancels_only_the_matching_capture_session() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let mut transitions = InteractiveSpaceTransitions::default();

    assert!(transitions.begin(
        &old,
        ButtonId::GestureButton,
        GestureDirection::Right,
        &Action::NextDesktop,
    ));
    assert!(transitions.begin(
        &replacement,
        ButtonId::GestureButton,
        GestureDirection::Right,
        &Action::PreviousDesktop,
    ));
    assert_eq!(
        transitions.cancel_session(&old),
        vec![SpaceSwipeFrame {
            progress_x: 0.0,
            phase: openlogi_inject::InteractiveSpacePhase::Cancelled,
        }]
    );
    assert!(
        transitions
            .motion(&replacement, ButtonId::GestureButton, 30)
            .is_some(),
        "cancelling a stale epoch must not erase its replacement transition"
    );
}

#[test]
fn interactive_space_transition_rejects_non_desktop_actions() {
    let session = HidppSessionId::with_epoch("mouse-a", 7);
    let mut transitions = InteractiveSpaceTransitions::default();

    assert!(!transitions.begin(
        &session,
        ButtonId::GestureButton,
        GestureDirection::Right,
        &Action::NextTab,
    ));
    assert!(
        transitions
            .motion(&session, ButtonId::GestureButton, 120)
            .is_none(),
        "ordinary gestures remain one-shot actions"
    );
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
            .advance(rotation(threshold), &Action::VolumeUp, scale(), now,),
        WheelOutput::FireAction
    );
    assert_eq!(
        wheels.for_session(&replacement).advance(
            rotation(threshold),
            &Action::VolumeUp,
            scale(),
            now,
        ),
        WheelOutput::FireAction,
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

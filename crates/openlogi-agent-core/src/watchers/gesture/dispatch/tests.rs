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

#[cfg(not(target_os = "macos"))]
#[test]
fn cancel_zoom_preserves_ordinary_wheel_progress() {
    let session = HidppSessionId::with_epoch("mouse-a", 7);
    let mut wheels = SessionWheels::default();
    let now = Instant::now();
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    assert_eq!(
        wheels.for_session(&session).advance(
            rotation(threshold - 1),
            &Action::VolumeUp,
            scale(),
            now
        ),
        WheelOutput::Idle
    );
    wheels.cancel_zoom_session(&session);
    assert_eq!(
        wheels
            .for_session(&session)
            .advance(rotation(1), &Action::VolumeUp, scale(), now),
        WheelOutput::FireAction(1)
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn stale_epoch_zoom_cancel_preserves_successor_progress() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let mut wheels = SessionWheels::default();
    let now = Instant::now();
    for session in [&old, &replacement] {
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(100), &Action::ZoomIn, scale(), now),
            WheelOutput::Idle
        );
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(4), &Action::ZoomIn, scale(), now),
            WheelOutput::Idle
        );
    }
    wheels.cancel_zoom_session(&old);
    assert_eq!(
        wheels
            .for_session(&replacement)
            .advance(rotation(1), &Action::ZoomIn, scale(), now),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(100), &Action::ZoomIn, scale(), now),
        WheelOutput::Idle
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn clear_all_zoom_restarts_each_sessions_recognition() {
    let sessions = [
        HidppSessionId::with_epoch("mouse-a", 7),
        HidppSessionId::with_epoch("mouse-b", 8),
    ];
    let mut wheels = SessionWheels::default();
    let now = Instant::now();
    for session in &sessions {
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(100), &Action::ZoomIn, scale(), now),
            WheelOutput::Idle
        );
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(4), &Action::ZoomIn, scale(), now),
            WheelOutput::Idle
        );
    }
    wheels.clear_zoom();
    for session in &sessions {
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(100), &Action::ZoomIn, scale(), now),
            WheelOutput::Idle
        );
        assert_eq!(
            wheels
                .for_session(session)
                .advance(rotation(5), &Action::ZoomIn, scale(), now),
            WheelOutput::FireAction(1)
        );
    }
}

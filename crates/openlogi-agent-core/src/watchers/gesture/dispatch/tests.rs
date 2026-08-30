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

#[test]
fn touchpad_stroke_freezes_bindings_from_its_first_frame() {
    use openlogi_core::touchpad::TouchContact;

    let frame = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let mut runtime = TouchpadRuntime::default();
    let first_profile = BTreeMap::from([(trigger, Action::Copy)]);
    let replacement_profile = BTreeMap::from([(trigger, Action::Paste)]);

    assert_eq!(runtime.update(&frame, &first_profile, true), None);
    // A foreground-app change can replace the live plan before lift. The tap
    // must still resolve against the profile active when the stroke began.
    assert_eq!(
        runtime.end(true),
        Some((ButtonId::TouchpadTwoFingerTap, Action::Copy))
    );

    assert_eq!(runtime.update(&frame, &replacement_profile, true), None);
    assert_eq!(
        runtime.end(true),
        Some((ButtonId::TouchpadTwoFingerTap, Action::Paste))
    );
}

#[test]
fn diagnostic_touchpad_stroke_cannot_fire_if_management_enables_mid_stroke() {
    use openlogi_core::touchpad::TouchContact;

    let frame = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let bindings = BTreeMap::from([(trigger, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    assert_eq!(runtime.update(&frame, &bindings, false), None);
    assert_eq!(runtime.end(true), None);

    assert_eq!(runtime.update(&frame, &bindings, true), None);
    assert_eq!(runtime.end(true), Some((trigger, Action::Copy)));
}

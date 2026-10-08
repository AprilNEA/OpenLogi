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
fn shifted_presses_use_the_gshift_layer_and_fall_back_to_normal() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding("g502", ButtonId::G8, Binding::Single(Action::Copy));
    config.set_gshift_binding("g502", ButtonId::G8, Some(Action::Paste));
    let plan = crate::capture_plan::plan_for_device(
        &config,
        openlogi_core::device_order::PhysicalDeviceKey::parse("receiver:cafe:slot:1")
            .expect("physical key"),
        "g502",
        openlogi_hid::DeviceRoute::Bolt {
            receiver_uid: "cafe".into(),
            slot: 1,
        },
        None,
        0,
        true,
    )
    .dispatch;

    let action = |button, shifted| press_binding(&plan, button, shifted).map(Binding::click_action);
    assert_eq!(action(ButtonId::G8, false), Some(Action::Copy));
    assert_eq!(action(ButtonId::G8, true), Some(Action::Paste));
    assert_eq!(action(ButtonId::G7, true), Some(Action::PreviousDpiPreset));
}

#[test]
fn shift_layer_stays_on_until_every_shift_button_is_released() {
    let session = HidppSessionId::with_epoch("mouse-a", 1);
    let mut held = HeldShift::default();
    held.press(&session, ButtonId::Forward);
    held.press(&session, ButtonId::G9);
    assert!(held.release(&session, ButtonId::G9));
    assert!(held.active(&session));
    assert!(!held.release(&session, ButtonId::G7));
    assert!(held.release(&session, ButtonId::Forward));
    assert!(!held.active(&session));
}

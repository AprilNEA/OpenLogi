use openlogi_hid::thumbwheel::WheelResolution;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::*;
use openlogi_core::binding::GestureDirection;
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};
use crate::runtime::testing::{ButtonRuntimeEvent, recording_button_runtime};

fn recv_event(receiver: &mpsc::Receiver<ButtonRuntimeEvent>) -> ButtonRuntimeEvent {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("dispatcher should emit a button event")
}

fn assert_action(receiver: &mpsc::Receiver<ButtonRuntimeEvent>, expected: &Action) {
    let ButtonRuntimeEvent::Triggered { action, .. } = recv_event(receiver) else {
        panic!("expected a dispatched action");
    };
    assert_eq!(&action, expected);
}

#[test]
fn captured_volume_motion_dispatches_until_release_or_session_cancellation() {
    for button in [ButtonId::Back, ButtonId::GestureButton] {
        let (mut actions, events) = recording_button_runtime();
        let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
            false,
            openlogi_core::config::VerticalScrollSensitivity::default(),
        )))
        .unwrap();
        let mut dispatcher = InputDispatcher::new(GestureOutputs::new(
            actions.dispatcher(),
            scroll.input(),
            Arc::default(),
        ));
        let bindings = BTreeMap::from([
            (GestureDirection::Up, Action::VolumeUp),
            (GestureDirection::Down, Action::VolumeDown),
        ]);
        let mut plan = DispatchPlan {
            config_key: "mouse-a".into(),
            bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            side_gesture_bindings: BTreeMap::new(),
            thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
            pointer_target: Some(openlogi_hook::PointerTarget::Unavailable),
        };
        if button == ButtonId::Back {
            plan.side_gesture_bindings.insert(button, bindings);
        } else {
            plan.gesture_bindings.insert(button, bindings);
        }
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        let motion = |dy| CapturedInput::GestureMotion { button, dx: 0, dy };

        dispatcher.dispatch(&session, &plan, CapturedInput::ButtonDown(button));
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Started(_)
        ));
        dispatcher.dispatch(
            &session,
            &plan,
            CapturedInput::Gesture(button, GestureDirection::Up),
        );
        assert_action(&events, &Action::VolumeUp);
        dispatcher.dispatch(&session, &plan, motion(-100));
        assert_action(&events, &Action::VolumeUp);
        assert_action(&events, &Action::VolumeUp);
        dispatcher.dispatch(&session, &plan, motion(-49));
        dispatcher.dispatch(&session, &plan, motion(50));
        assert_action(&events, &Action::VolumeDown);

        dispatcher.dispatch(&session, &plan, CapturedInput::ButtonUp(button));
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Ended { .. }
        ));
        dispatcher.dispatch(&session, &plan, motion(100));
        dispatcher.dispatch(&session, &plan, CapturedInput::ButtonDown(button));
        // The next lifecycle event is an ordered barrier: no motion action was
        // emitted after release, without relying on a sleep or an empty queue.
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Started(_)
        ));
        dispatcher.dispatch(&session, &plan, motion(-100));
        dispatcher.dispatch(
            &session,
            &plan,
            CapturedInput::Gesture(button, GestureDirection::Down),
        );
        assert_action(&events, &Action::VolumeDown);

        dispatcher.cancel_session(&session);
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Ended { .. }
        ));
        dispatcher.dispatch(&session, &plan, motion(-100));
        let replacement = HidppSessionId::with_epoch("mouse-a", 8);
        dispatcher.dispatch(&replacement, &plan, CapturedInput::ButtonDown(button));
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Started(_)
        ));
        dispatcher.dispatch(&replacement, &plan, motion(-100));
        dispatcher.dispatch(&replacement, &plan, CapturedInput::ButtonUp(button));
        assert!(matches!(
            recv_event(&events),
            ButtonRuntimeEvent::Ended { .. }
        ));
        actions.shutdown();
        scroll.shutdown();
    }
}

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

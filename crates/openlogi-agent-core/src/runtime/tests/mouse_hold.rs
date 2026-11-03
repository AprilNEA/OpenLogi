//! Drives real producer inputs and worker events, not fabricated press state.

use super::super::button::CancelReason;
use super::*;
use openlogi_core::binding::MouseButton;

struct MouseHoldHarness {
    owner: ButtonRuntimeOwner,
    input: ButtonInputHandle,
    events: std::sync::mpsc::Receiver<ButtonRuntimeEvent>,
    handler: ButtonEventHandler<RecordingOutput>,
    output: RecordingOutput,
    session: HidppSessionId,
}

impl MouseHoldHarness {
    fn new() -> Self {
        let (sent, events) = std::sync::mpsc::channel();
        let owner =
            ButtonRuntimeOwner::spawn(move |event| sent.send(event).expect("test receiver"))
                .expect("button worker");
        let input = owner.input();
        let output = RecordingOutput::default();
        Self {
            owner,
            input,
            events,
            handler: ButtonEventHandler {
                executor: executor_with(Arc::default()),
                held: HeldActions::new(output.clone()),
                held_dpi: HeldDpiShifts::default(),
            },
            output,
            session: HidppSessionId::with_epoch("test-mouse", 7),
        }
    }

    fn next(&mut self, inspect: impl FnOnce(&ButtonRuntimeEvent)) {
        let event = self
            .events
            .recv_timeout(Duration::from_secs(1))
            .expect("worker event");
        inspect(&event);
        self.handler.handle(event);
    }

    fn press(&mut self) -> PressToken {
        let token = self
            .input
            .try_hidpp_down(
                &self.session,
                ButtonId::DpiToggle,
                Some(&Binding::Single(Action::MiddleClick)),
                ActionDispatchTarget::Keyboard,
            )
            .expect("HID++ down");
        self.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Started(_))));
        assert!(self.handler.held.contains(&token));
        token
    }

    fn expect_end(&mut self, token: &PressToken, expected: EndReason) {
        self.next(|event| {
            let ButtonRuntimeEvent::Ended { press, reason } = event else {
                panic!("the physical press must end before the next action");
            };
            assert_eq!(press.token(), token);
            assert_eq!(*reason, expected);
        });
        assert!(!self.handler.held.contains(token));
    }
}

#[test]
fn hidpp_middle_click_stays_held_until_its_matching_up() {
    let mut harness = MouseHoldHarness::new();
    let token = harness.press();
    assert!(
        harness
            .input
            .try_hidpp_up(&harness.session, ButtonId::DpiToggle)
    );
    harness.expect_end(&token, EndReason::Released);
    assert_eq!(
        harness.output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle)
        ]
    );
}

#[test]
fn middle_click_terminal_cancellations_release_the_output_owner() {
    for reason in [
        CancelReason::SourceEnded,
        CancelReason::Invalidated,
        CancelReason::StaleHold,
        CancelReason::Shutdown,
    ] {
        let mut harness = MouseHoldHarness::new();
        let token = harness.press();
        match reason {
            CancelReason::SourceEnded => harness.input.cancel_hidpp_session(&harness.session),
            CancelReason::Invalidated => harness.input.invalidate_all(),
            CancelReason::StaleHold => harness.input.cancel_stale_press(&token),
            CancelReason::Shutdown => assert!(harness.owner.shutdown()),
            CancelReason::RepeatedDown => unreachable!("covered by the repress test"),
        }
        harness.expect_end(&token, EndReason::Canceled(reason));
        assert_eq!(
            harness.output.events(),
            [
                OutputEvent::MouseDown(MouseButton::Middle),
                OutputEvent::MouseUp(MouseButton::Middle)
            ]
        );
    }
}

#[test]
fn hooked_middle_click_releases_when_capture_is_interrupted() {
    let mut harness = MouseHoldHarness::new();
    let token = harness
        .input
        .try_hook_down_with_target(
            ButtonId::Back,
            Some(&Binding::Single(Action::MiddleClick)),
            ActionDispatchTarget::Keyboard,
        )
        .expect("OS-hook down");
    harness.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Started(_))));
    assert!(harness.handler.held.contains(&token));
    harness.input.cancel_hooks();
    harness.expect_end(&token, EndReason::Canceled(CancelReason::SourceEnded));
    assert_eq!(
        harness.output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle)
        ]
    );
}

#[test]
fn middle_click_repress_ends_old_output_before_starting_the_new_owner() {
    let mut harness = MouseHoldHarness::new();
    let first = harness.press();
    let second = harness
        .input
        .try_hidpp_down(
            &harness.session,
            ButtonId::DpiToggle,
            Some(&Binding::Single(Action::MiddleClick)),
            ActionDispatchTarget::Keyboard,
        )
        .expect("replacement down");
    harness.expect_end(&first, EndReason::Canceled(CancelReason::RepeatedDown));
    harness.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Started(_))));
    assert!(harness.handler.held.contains(&second));
    assert!(
        harness
            .input
            .try_hidpp_up(&harness.session, ButtonId::DpiToggle)
    );
    harness.expect_end(&second, EndReason::Released);
    assert_eq!(
        harness.output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle),
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle)
        ]
    );
}

#[test]
fn canceling_one_middle_click_source_keeps_the_other_press_owned() {
    let mut harness = MouseHoldHarness::new();
    let first = harness.press();
    let other_session = HidppSessionId::with_epoch("other-mouse", 8);
    let second = harness
        .input
        .try_hidpp_down(
            &other_session,
            ButtonId::DpiToggle,
            Some(&Binding::Single(Action::MiddleClick)),
            ActionDispatchTarget::Keyboard,
        )
        .expect("second source down");
    harness.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Started(_))));
    harness.input.cancel_hidpp_session(&harness.session);
    harness.expect_end(&first, EndReason::Canceled(CancelReason::SourceEnded));
    assert!(harness.handler.held.contains(&second));
    assert!(
        harness
            .input
            .try_hidpp_up(&other_session, ButtonId::DpiToggle)
    );
    harness.expect_end(&second, EndReason::Released);
}

#[test]
fn gesture_middle_click_is_a_tap_not_a_lifecycle_held_output() {
    let mut harness = MouseHoldHarness::new();
    let token = harness
        .input
        .try_hidpp_down(
            &harness.session,
            ButtonId::GestureButton,
            None,
            ActionDispatchTarget::Keyboard,
        )
        .expect("gesture down");
    harness.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Started(_))));
    assert!(
        harness
            .input
            .try_trigger_while_pressed(&token, &Action::MiddleClick)
    );
    harness.next(|event| assert!(matches!(event, ButtonRuntimeEvent::Triggered { .. })));
    assert!(!harness.handler.held.contains(&token));
    assert_eq!(
        harness.output.events(),
        [OutputEvent::Tap(Action::MiddleClick)]
    );
    assert!(
        harness
            .input
            .try_hidpp_up(&harness.session, ButtonId::GestureButton)
    );
    harness.expect_end(&token, EndReason::Released);
}

#[test]
fn dropping_the_middle_click_handler_releases_the_output() {
    let mut harness = MouseHoldHarness::new();
    harness.press();
    let output = harness.output.clone();
    drop(harness);
    assert_eq!(
        output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle),
        ]
    );
}

#[test]
fn middle_click_output_releases_during_unwinding() {
    let output = RecordingOutput::default();
    let result = std::panic::catch_unwind(|| {
        let mut held = HeldActions::new(output.clone());
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        assert!(held.start(&press, &Action::MiddleClick, ActionDispatchTarget::Keyboard));
        panic!("simulate action-worker unwinding");
    });
    assert!(result.is_err(), "the test must exercise unwinding");
    assert_eq!(
        output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle),
        ]
    );
}

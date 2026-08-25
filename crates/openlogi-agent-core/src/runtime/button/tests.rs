//! Regression tests for the source-independent button lifecycle.

use std::time::Instant;

use super::*;

fn hook_press(id: u64, button: ButtonId) -> ActivePress {
    ActivePress {
        token: PressToken::hook_for_test(id, button),
        action: Some(Action::Copy),
    }
}

fn recv_event(receiver: &mpsc::Receiver<ButtonRuntimeEvent>) -> ButtonRuntimeEvent {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("button worker should emit an event")
}

#[test]
fn release_returns_the_exact_active_press_once() {
    let mut state = ButtonState::default();
    let press = hook_press(1, ButtonId::Back);
    assert!(state.press(press.clone()).is_none());
    assert_eq!(state.release(&press.token.key), Some(press.clone()));
    assert_eq!(state.release(&press.token.key), None);
}

#[test]
fn repress_replaces_the_old_press_with_a_new_identity() {
    let mut state = ButtonState::default();
    let old = hook_press(1, ButtonId::Back);
    let new = hook_press(2, ButtonId::Back);
    state.press(old.clone());

    assert_eq!(state.press(new.clone()), Some(old.clone()));
    assert!(state.active(&old.token).is_none());
    assert_eq!(state.active(&new.token), Some(&new));
}

#[test]
fn cancellation_is_scoped_to_one_session() {
    let mut state = ButtonState::default();
    let first_source = ButtonSource::Hidpp(HidppSessionId::new("mouse-a", 7));
    let second_source = ButtonSource::Hidpp(HidppSessionId::new("mouse-b", 3));
    let first = ActivePress {
        token: PressToken {
            id: PressId(1),
            key: PressKey::new(first_source.clone(), ButtonId::Back),
            generation: 0,
        },
        action: None,
    };
    let second = ActivePress {
        token: PressToken {
            id: PressId(2),
            key: PressKey::new(second_source, ButtonId::Back),
            generation: 0,
        },
        action: None,
    };
    state.press(first.clone());
    state.press(second.clone());

    assert_eq!(state.cancel_source(&first_source), vec![first]);
    assert_eq!(state.release(&second.token.key), Some(second));
}

#[test]
fn hook_cancellation_leaves_hidpp_presses_active() {
    let mut state = ButtonState::default();
    let hook = hook_press(1, ButtonId::Back);
    let hidpp = ActivePress {
        token: PressToken {
            id: PressId(2),
            key: PressKey::new(
                ButtonSource::Hidpp(HidppSessionId::new("mouse-a", 7)),
                ButtonId::Forward,
            ),
            generation: 0,
        },
        action: None,
    };
    state.press(hook.clone());
    state.press(hidpp.clone());

    assert_eq!(state.cancel_hooks(), vec![hook]);
    assert_eq!(state.release(&hidpp.token.key), Some(hidpp));
}

#[test]
fn stale_token_cannot_trigger_after_same_key_repress() {
    let (sent, received) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let input = owner.input();

    let old = input
        .try_hook_down(ButtonId::Back, None)
        .expect("first down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Started(_)
    ));
    let new = input
        .try_hook_down(ButtonId::Back, None)
        .expect("replacement down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Ended {
            reason: EndReason::Canceled(CancelReason::RepeatedDown),
            ..
        }
    ));
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Started(_)
    ));

    assert!(input.try_trigger_while_pressed(&old, &Action::Copy));
    assert!(input.try_trigger_while_pressed(&new, &Action::Paste));
    let ButtonRuntimeEvent::Triggered { press, action } = recv_event(&received) else {
        panic!("only the replacement token should trigger");
    };
    assert_eq!(press.token, new);
    assert_eq!(action, Action::Paste);
    assert!(owner.shutdown());
}

#[test]
fn source_cancellation_invalidates_queued_gesture_work() {
    let (sent, received) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let input = owner.input();
    let session = HidppSessionId::new("mouse-a", 7);
    let token = input
        .try_hidpp_down(&session, ButtonId::Back, None)
        .expect("down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Started(_)
    ));

    input.cancel_hidpp_session(&session);
    assert!(input.try_trigger_while_pressed(&token, &Action::Copy));
    let sentinel = input
        .try_hook_down(ButtonId::Forward, None)
        .expect("sentinel down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Ended {
            reason: EndReason::Canceled(CancelReason::SourceEnded),
            ..
        }
    ));
    let ButtonRuntimeEvent::Started(started) = recv_event(&received) else {
        panic!("canceled gesture work must not run before the sentinel");
    };
    assert_eq!(started.token, sentinel);
    assert!(owner.shutdown());
}

#[test]
fn stale_hold_cancellation_emits_a_typed_terminal_event() {
    let (sent, received) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let input = owner.input();
    let stale = input
        .try_hook_down(ButtonId::Back, None)
        .expect("down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Started(_)
    ));

    input.cancel_stale_press(&stale);
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Ended {
            reason: EndReason::Canceled(CancelReason::StaleHold),
            ..
        }
    ));
    assert!(owner.shutdown());
}

#[test]
fn invalidation_rejects_old_tokens_and_cancels_active_presses() {
    let (sent, received) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let input = owner.input();
    let token = input
        .try_hook_down(ButtonId::Back, None)
        .expect("down should be queued");
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Started(_)
    ));

    input.invalidate_all();
    assert!(!input.try_trigger_while_pressed(&token, &Action::Copy));
    assert!(matches!(
        recv_event(&received),
        ButtonRuntimeEvent::Ended {
            reason: EndReason::Canceled(CancelReason::Invalidated),
            ..
        }
    ));
    assert!(owner.shutdown());
}

#[test]
fn worker_drops_input_queued_before_generation_invalidation() {
    let (commands, queued) = mpsc::sync_channel(1);
    commands
        .send(ButtonCommand::Input {
            generation: 0,
            input: ButtonInput::Down(hook_press(1, ButtonId::Back)),
        })
        .expect("test queue should accept the command");
    drop(commands);
    let (_shutdown, shutdown) = mpsc::channel();
    let generation = AtomicU64::new(1);
    let (sent, received) = mpsc::channel();

    run_worker(&queued, &shutdown, &generation, &|event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    });

    assert!(
        received.try_recv().is_err(),
        "an old profile's queued down must not start a lifecycle"
    );
}

#[test]
fn worker_emits_balanced_shutdown_and_rejects_later_input() {
    let (sent, received) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        sent.send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let input = owner.input();

    let token = input
        .try_hook_down(ButtonId::Back, Some(&Action::Copy))
        .expect("down should be queued");
    let ButtonRuntimeEvent::Started(started) = recv_event(&received) else {
        panic!("expected a started event");
    };
    assert_eq!(started.token, token);
    assert!(owner.shutdown());
    let ButtonRuntimeEvent::Ended { press, reason } = recv_event(&received) else {
        panic!("expected an ended event");
    };
    assert_eq!(press.token, token);
    assert_eq!(reason, EndReason::Canceled(CancelReason::Shutdown));
    assert!(input.try_hook_down(ButtonId::Forward, None).is_none());
}

#[test]
fn shutdown_deadline_includes_a_blocked_terminal_handler() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut owner = ButtonRuntimeOwner::spawn(move |event| {
        if matches!(event, ButtonRuntimeEvent::Started(_)) {
            entered_tx
                .send(())
                .expect("test receiver should stay connected");
            let _ = release_rx.recv();
        }
    })
    .expect("button worker should start");
    let input = owner.input();
    assert!(input.try_hook_down(ButtonId::Back, None).is_some());
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("handler should start");

    let started = Instant::now();
    assert!(!owner.shutdown_with_timeout(Duration::from_millis(20)));
    assert!(started.elapsed() < Duration::from_millis(200));
    let _ = release_tx.send(());
}

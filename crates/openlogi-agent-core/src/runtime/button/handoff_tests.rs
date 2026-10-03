use super::*;

#[test]
fn handoff_waits_for_terminal_handlers_and_preserves_other_sources() {
    let now = Instant::now();
    let hook = ActivePress {
        token: PressToken::hook_for_test(1, ButtonId::Back),
        behavior: PressBehavior::new(
            Some(&Binding::LongPress(
                openlogi_core::binding::LongPressBinding::new(Action::Copy, Action::Paste),
            )),
            now.checked_sub(LONG_PRESS_THRESHOLD).unwrap(),
        ),
        target: ActionDispatchTarget::Keyboard,
        validity: None,
    };
    let mut keyboard = hook.clone();
    keyboard.token.key.control = PressControl::Key(79);
    keyboard.behavior = PressBehavior::Immediate(Action::None);
    let mut hidpp = keyboard.clone();
    hidpp.token.key.source = ButtonSource::Hidpp(HidppSessionId::new("another-device"));
    let mut state = ButtonState::default();
    for press in [hook.clone(), keyboard.clone(), hidpp.clone()] {
        process_input(&mut state, ButtonInput::Down(press), &mut |_| {});
    }
    let (sent, queue) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let (done, mut acknowledged) = tokio::sync::oneshot::channel();
    sent.try_send(ButtonCommand::Drain(ButtonDrain::HookButtons, done))
        .unwrap();
    let (_stop, shutdown) = mpsc::channel();
    let mut terminal_finished = false;
    assert!(!settle_due_long_presses(
        &queue,
        &shutdown,
        &AtomicU64::new(0),
        &mut 0,
        &mut state,
        None,
        &mut |event| match event {
            ButtonRuntimeEvent::Ended { press, reason } => {
                assert_eq!(press.token, hook.token);
                assert_eq!(reason, EndReason::Canceled(CancelReason::SourceEnded));
                assert!(
                    matches!(
                        acknowledged.try_recv(),
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                    ),
                    "deferred terminal handlers must finish before acknowledgement"
                );
                terminal_finished = true;
            }
            ButtonRuntimeEvent::Drained(done) => {
                assert!(terminal_finished);
                done.send(()).unwrap();
            }
            _ => panic!("handoff must not fire a canceled short or long action"),
        },
    ));
    acknowledged.try_recv().unwrap();
    assert_eq!(state.active.len(), 2);
    assert_eq!(state.release(&keyboard.token.key), Some(keyboard));
    assert_eq!(state.release(&hidpp.token.key), Some(hidpp));
}

#[tokio::test]
async fn plugin_replacement_waits_for_terminal_handlers_and_uses_a_new_input_token() {
    let (events, observed) = mpsc::channel();
    let (ending, ended) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut first_end = true;
    let mut runtime = ButtonRuntimeOwner::spawn(move |event| match event {
        ButtonRuntimeEvent::Drained(done) => {
            done.send(()).unwrap();
        }
        event => {
            if matches!(event, ButtonRuntimeEvent::Ended { .. }) && first_end {
                first_end = false;
                ending.send(()).unwrap();
                released.recv().unwrap();
            }
            events.send(event).unwrap();
        }
    })
    .unwrap();
    let input = runtime.input();
    let session = SessionId {
        endpoint: openlogi_core::peripheral::EndpointId("same-attachment".into()),
        generation: 1,
    };
    let previous = input.peripheral_session(&session);
    let capability = CapabilityId::try_new("input-remap/main").unwrap();
    let control = ControlId::try_new("button").unwrap();
    let press = openlogi_core::peripheral::InputTransition::Press;
    assert!(previous.send(&capability, &control, press, &Action::None));
    assert!(matches!(
        observed.recv().unwrap(),
        ButtonRuntimeEvent::Started(_)
    ));
    previous.cancel();
    ended.recv().unwrap();
    let mut drain = Box::pin(input.drain(ButtonDrain::Queued));
    assert!(futures_lite::future::poll_once(&mut drain).await.is_none());
    release.send(()).unwrap();
    drain.await.unwrap();
    assert!(matches!(
        observed.recv().unwrap(),
        ButtonRuntimeEvent::Ended { .. }
    ));
    let successor = input.peripheral_session(&session);
    assert!(successor.send(&capability, &control, press, &Action::None));
    assert!(matches!(
        observed.recv().unwrap(),
        ButtonRuntimeEvent::Started(_)
    ));
    previous.cancel();
    assert!(!previous.send(&capability, &control, press, &Action::None));
    assert!(successor.send(
        &capability,
        &control,
        openlogi_core::peripheral::InputTransition::Release,
        &Action::None,
    ));
    assert!(matches!(
        observed.recv().unwrap(),
        ButtonRuntimeEvent::Ended {
            reason: EndReason::Released,
            ..
        }
    ));
    assert!(runtime.shutdown());
    assert!(
        observed.try_recv().is_err(),
        "old tokens cannot cancel the replacement"
    );
}

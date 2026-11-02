use super::*;
use crate::receiver_access::ReceiverAccess;
use openlogi_core::binding::Action;
use openlogi_hid::{CaptureChannelSlot, ChannelRegistry};

/// The Search key (`0x1b04` control `0x00d4`).
const SEARCH_KEY: ButtonId = ButtonId::control(0x00d4);

fn target() -> KeyboardTarget {
    KeyboardTarget {
        route: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc548,
        },
    }
}

/// A published spec for the keyboard at `route`, named `config_key`.
fn spec_at(config_key: &str, route: DeviceRoute) -> KeyboardSpec {
    KeyboardSpec {
        config_key: config_key.to_owned(),
        route,
        wanted: dispatch(Action::MissionControl).keys.controls,
        grid: dispatch(Action::MissionControl).keys.grid,
        bindings: dispatch(Action::MissionControl).bindings,
    }
}

/// A second keyboard: a keypad wired next to the receiver keyboard.
fn keypad_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc354,
    }
}

fn slots(
    entries: impl IntoIterator<Item = (DeviceRoute, KeyboardSlot)>,
) -> HashMap<DeviceRoute, KeyboardSlot> {
    entries.into_iter().collect()
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("keyboard-a", epoch)
}

fn dispatch(action: Action) -> KeyboardDispatchPlan {
    KeyboardDispatchPlan {
        config_key: "keyboard-a".to_owned(),
        bindings: BTreeMap::from([(SEARCH_KEY, Binding::Single(action))]),
        keys: KeyboardWanted {
            controls: BTreeMap::from([(0x00d4, SEARCH_KEY)]),
            grid: BTreeMap::new(),
        },
    }
}

fn live_session(epoch: u64) -> RunningKeyboardSession {
    let (stop, _rx) = oneshot::channel();
    CaptureSession::active(
        session_id(epoch),
        target(),
        dispatch(Action::MissionControl),
        stop,
    )
}

fn draining_session(epoch: u64) -> RunningKeyboardSession {
    let mut session = live_session(epoch);
    assert_eq!(
        session.reconcile_with(None, |_, _| CaptureSessionStop::Shutdown),
        ReconcileAction::Retiring
    );
    session
}

#[tokio::test]
async fn publication_and_receiver_request_change_wanted_state_immediately() {
    let (spec_tx, mut spec_rx) = watch::channel(Arc::new(Vec::new()));
    let access = ReceiverAccess::default();
    let mut requests = access.subscribe_requests();

    spec_tx.send_replace(Arc::new(vec![
        spec_at("keyboard-a", target().route),
        spec_at("keypad", keypad_route()),
    ]));
    spec_rx
        .changed()
        .await
        .expect("spec publication should remain open");
    let wanted = wanted_sessions(*requests.borrow(), &spec_rx.borrow());
    assert_eq!(
        wanted
            .iter()
            .map(|(target, dispatch)| (&target.route, dispatch.config_key.as_str()))
            .collect::<Vec<_>>(),
        [(&target().route, "keyboard-a"), (&keypad_route(), "keypad")],
        "every published keyboard is wanted, each with its own dispatch"
    );

    let session_lease = access
        .try_acquire_for_session()
        .expect("the test session should hold shared access");
    let exclusive = tokio::spawn({
        let access = access.clone();
        async move {
            access
                .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
                .await
        }
    });
    requests
        .changed()
        .await
        .expect("request publication should remain open");
    assert!(
        wanted_sessions(*requests.borrow(), &spec_rx.borrow()).is_empty(),
        "a queued request should retire every keyboard's capture without waiting for a tick"
    );

    exclusive.abort();
    let _ = exclusive.await;
    drop(session_lease);
}

#[test]
fn accepts_inputs_from_the_current_session_until_teardown_finishes() {
    assert!(live_session(7).owns(&session_id(7)));
    assert!(
        !live_session(7).owns(&session_id(6)),
        "a superseded session's queued input is stale"
    );
    assert!(
        draining_session(7).owns(&session_id(7)),
        "the draining keyboard remains the sole owner until restore and ordered Done"
    );
}

#[test]
fn binding_changes_refresh_without_rearming_hardware() {
    let mut session = live_session(7);
    let current_target = session.target().clone();
    let new_dispatch = dispatch(Action::ShowDesktop);

    assert_eq!(
        session.reconcile_with(Some((&current_target, &new_dispatch)), |_, _| {
            CaptureSessionStop::Shutdown
        }),
        ReconcileAction::DispatchChanged
    );
    assert!(session.is_active());
    assert_eq!(session.dispatch(), &new_dispatch);
}

/// A per-app profile that binds one more key changes the plan, not the
/// target (#2140): the session stays active, and the new divert set reaches
/// it. A plan with the same divert set publishes nothing.
#[tokio::test]
async fn divert_set_changes_reach_the_running_session_in_place() {
    let access = ReceiverAccess::default();
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (ring, _ring_rx) = mpsc::unbounded_channel();
    let mut actions = crate::runtime::ActionRuntime::new(
        Arc::default(),
        Arc::default(),
        DeviceAccess {
            channel: CaptureChannelSlot::default(),
            registry: ChannelRegistry::default(),
            receiver_access: access,
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        },
        ring,
    )
    .unwrap();
    let keys = watch::Sender::new(dispatch(Action::MissionControl).keys);
    let mut session_view = keys.subscribe();
    let mut session = live_session(7);
    let mut per_app = dispatch(Action::ShowDesktop);
    let backlight_down = ButtonId::control(0x01a1);
    per_app
        .bindings
        .insert(backlight_down, Binding::Single(Action::Copy));
    per_app.keys.controls.insert(0x01a1, backlight_down);
    // A keypad grid key bound by the same profile joins in place too.
    let grid_key = ButtonId::grid(openlogi_core::binding::GridKey::TopLeft);
    per_app
        .bindings
        .insert(grid_key, Binding::Single(Action::Paste));
    per_app.keys.grid.insert(1, grid_key);

    reconcile_session(
        &mut session,
        Some((&target(), &per_app)),
        &actions.dispatcher(),
        &keys,
    );
    assert!(session.is_active(), "a divert-set change must not restart");
    assert_eq!(session.dispatch(), &per_app);
    assert!(session_view.has_changed().unwrap());
    assert_eq!(*session_view.borrow_and_update(), per_app.keys);

    let mut same_keys = per_app.clone();
    same_keys
        .bindings
        .insert(backlight_down, Binding::Single(Action::Paste));
    reconcile_session(
        &mut session,
        Some((&target(), &same_keys)),
        &actions.dispatcher(),
        &keys,
    );
    assert!(
        !session_view.has_changed().unwrap(),
        "an action-only change must not write firmware"
    );
    actions.shutdown();
}

#[test]
fn target_changes_freeze_dispatch_until_teardown_finishes() {
    let mut session = live_session(7);
    let old_dispatch = session.dispatch().clone();
    let replacement = KeyboardTarget {
        route: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc52b,
        },
    };
    let new_dispatch = dispatch(Action::ShowDesktop);

    assert!(
        replacement != *session.target(),
        "the test must require different firmware capture"
    );
    assert_eq!(
        session.reconcile_with(Some((&replacement, &new_dispatch)), |_, _| {
            CaptureSessionStop::Shutdown
        }),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());
    assert_eq!(session.dispatch(), &old_dispatch);
}

#[test]
fn suspended_device_io_disables_retry_deadlines() {
    let retry_at = tokio::time::Instant::now() + RETRY_DELAY;
    let slots = slots([(
        target().route,
        KeyboardSlot::recovering(None, Some(retry_at)),
    )]);

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, &slots),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(ReceiverRequestState::default(), false, &slots),
        None,
        "keyboard retries must stay dormant until visible resume",
    );
}

#[tokio::test(start_paused = true)]
async fn exclusive_receiver_access_disables_expired_recovery_deadlines() {
    let access = ReceiverAccess::default();
    let requests = access.subscribe_requests();
    let exclusive = access
        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
        .await;
    let restart_at = tokio::time::Instant::now();
    let slots = slots([(
        target().route,
        KeyboardSlot::recovering(None, Some(restart_at)),
    )]);

    assert_eq!(next_deadline(*requests.borrow(), true, &slots), None);
    drop(exclusive);
    assert_eq!(
        next_deadline(*requests.borrow(), true, &slots),
        Some(restart_at),
        "release makes the retry actionable without adding another backoff"
    );
}

#[test]
fn the_earliest_keyboard_retry_sets_the_deadline() {
    let now = tokio::time::Instant::now();
    let slots = slots([
        (
            target().route,
            KeyboardSlot::recovering(None, Some(now + RETRY_DELAY * 2)),
        ),
        (
            keypad_route(),
            KeyboardSlot::recovering(None, Some(now + RETRY_DELAY)),
        ),
    ]);

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, &slots),
        Some(now + RETRY_DELAY)
    );
}

#[test]
fn input_is_matched_to_its_own_keyboard_route() {
    let wanted = vec![
        (target(), dispatch(Action::MissionControl)),
        (
            KeyboardTarget {
                route: keypad_route(),
            },
            dispatch(Action::ShowDesktop),
        ),
    ];

    let (found, plan) = wanted_for(&wanted, &keypad_route()).expect("the keypad is wanted");
    assert_eq!(found.route, keypad_route());
    assert_eq!(plan, &dispatch(Action::ShowDesktop));
    assert!(
        wanted_for(
            &wanted,
            &DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 3,
            }
        )
        .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_manager_waits_for_control_events_and_shutdown_between_retries() {
    use futures_lite::future::poll_once;

    for shutdown_requested in [false, true] {
        let (spec_tx, specs) = watch::channel(Arc::new(vec![
            spec_at("keyboard-a", target().route),
            spec_at("keypad", keypad_route()),
        ]));
        let capture = CaptureChannelSlot::default();
        // A missing inventory channel makes the real session task fail without
        // opening hardware; ordered Done then drives normal restart recovery.
        let registry = ChannelRegistry::default();
        let access = ReceiverAccess::default();
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let (ring, _ring_rx) = mpsc::unbounded_channel();
        let device_access = DeviceAccess {
            channel: capture,
            registry,
            receiver_access: access.clone(),
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        };
        let mut actions = crate::runtime::ActionRuntime::new(
            Arc::default(),
            Arc::default(),
            device_access.clone(),
            ring,
        )
        .unwrap();
        let (shutdown_tx, shutdown) = oneshot::channel();
        let mut manager = std::pin::pin!(manage(KeyboardManagerContext {
            specs,
            receiver_requests: access.subscribe_requests(),
            access: device_access,
            dispatcher: actions.dispatcher(),
            shutdown,
        }));

        for _ in 0..3 {
            // Poll the actual manager and its detached forwarder/session tasks
            // through repeated failures. Each poll must return to event wait.
            for _ in 0..4 {
                assert!(poll_once(&mut manager).await.is_none());
                tokio::task::yield_now().await;
            }
            tokio::time::advance(RETRY_DELAY).await;
        }
        let before_event = tokio::time::Instant::now();
        if shutdown_requested {
            shutdown_tx.send(()).unwrap();
            assert!(matches!(manager.await, ManagerCompletion::Graceful));
        } else {
            drop(spec_tx);
            assert!(matches!(manager.await, ManagerCompletion::Unexpected));
        }
        assert_eq!(tokio::time::Instant::now(), before_event);
        actions.shutdown();
    }
}

/// Two published keyboards get two concurrent sessions, one per route, and
/// withdrawing one retires only its own.
#[tokio::test(start_paused = true)]
async fn each_published_keyboard_runs_its_own_session() {
    let access = ReceiverAccess::default();
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let device_access = DeviceAccess {
        channel: CaptureChannelSlot::default(),
        registry: ChannelRegistry::default(),
        receiver_access: access.clone(),
        device_io,
        transport_turns: crate::hardware::TransportTurns::default(),
    };
    let (ring, _ring_rx) = mpsc::unbounded_channel();
    let mut actions = crate::runtime::ActionRuntime::new(
        Arc::default(),
        Arc::default(),
        device_access.clone(),
        ring,
    )
    .unwrap();
    let (events, _event_rx) = mpsc::unbounded_channel();
    let channels = KeyboardSessionChannels {
        access: device_access,
        events,
    };
    let mut state = KeyboardManagerState::new(actions.dispatcher());
    let both = [
        spec_at("keyboard-a", target().route),
        spec_at("keypad", keypad_route()),
    ];

    state
        .reconcile(ReceiverRequestState::default(), true, &both, &channels)
        .await;
    for route in [target().route, keypad_route()] {
        assert!(
            state
                .slots
                .get(&route)
                .and_then(KeyboardSlot::session)
                .is_some(),
            "{route:?} has its own session"
        );
    }

    state
        .reconcile(ReceiverRequestState::default(), true, &both[..1], &channels)
        .await;
    assert!(
        state.slots[&target().route]
            .session()
            .is_some_and(CaptureSession::is_active),
        "the still-published keyboard keeps capturing"
    );
    assert!(
        state.slots[&keypad_route()]
            .session()
            .is_some_and(|session| !session.is_active()),
        "the withdrawn keypad's session retires"
    );
    actions.shutdown();
}

/// A session body that panics used to take its completion report down with
/// it, leaving the keyboard slot `Running` and the keys never re-armed.
#[tokio::test]
async fn a_session_body_that_panics_still_reports_done_after_its_inputs() {
    let id = session_id(3);
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (sink, session_rx) = mpsc::unbounded_channel();
    let forward = spawn_input_forwarder(target().route, id.clone(), session_rx, events.clone());
    let (never_restored, restore) = oneshot::channel::<Option<PendingCaptureRestore>>();
    let session_task = tokio::spawn(async move {
        sink.send(CapturedInput::ButtonPulse(SEARCH_KEY))
            .expect("the capture forwarder should remain open");
        drop(never_restored);
        // Dies with the sink still held, the way a panicking body does; the
        // unwind is what closes the forwarder.
        restore.await.expect("simulated capture panic")
    });

    complete_session(session_task, forward, events, target().route, id.clone()).await;

    match event_rx.recv().await {
        Some(KeyboardSessionEvent::Input(input)) => {
            assert_eq!(input.session, id);
            assert_eq!(input.route, target().route);
            assert_eq!(input.input, CapturedInput::ButtonPulse(SEARCH_KEY));
        }
        _ => panic!("the input accepted before the panic must still be forwarded"),
    }
    match event_rx.recv().await {
        Some(KeyboardSessionEvent::Done(done)) => {
            assert_eq!(done.session, id);
            assert_eq!(done.route, target().route);
            assert!(
                done.pending_restore.is_none(),
                "a panicked body has no restoration to hand over"
            );
        }
        _ => panic!("Done must follow the last input even when the body panicked"),
    }
}

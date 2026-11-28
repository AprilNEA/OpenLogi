use std::time::Duration;

use super::*;
use crate::watchers::retry::wait_for_deadline;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::config::{ThumbwheelSensitivity, VerticalScrollSensitivity};
use openlogi_hid::{CaptureChannelSlot, CaptureInputSink, DeviceRoute};

fn route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc548,
    }
}

fn physical_key() -> PhysicalDeviceKey {
    PhysicalDeviceKey::parse("unit:00000001").expect("fixture should be a physical key")
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("mouse-a", epoch)
}

fn plan_device() -> crate::capture_plan::PlanDevice<'static> {
    crate::capture_plan::PlanDevice {
        capabilities: Some(openlogi_core::device::Capabilities::from_feature_ids(&[
            0x2150,
        ])),
        physical_key: physical_key(),
        config_key: "mouse-a",
        route: route(),
        layout: None,
    }
}

fn plan() -> DeviceCapturePlan {
    crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        plan_device(),
        None,
        0,
        true,
    )
}

fn diverted_record(id: &HidppSessionId, hook_maps: &SharedHookMaps) -> DivertedButtonRecord {
    DivertedButtonRecord {
        hook_maps: Arc::clone(hook_maps),
        session: id.clone(),
        route: route(),
    }
}

fn capture_sink(
    id: &HidppSessionId,
    events: mpsc::UnboundedSender<SessionEvent>,
    hook_maps: &SharedHookMaps,
) -> (CaptureEvents, oneshot::Receiver<()>) {
    let (_, capture_plans) = watch::channel(Arc::new(CapturePublication::from_plans(vec![plan()])));
    let (drained, inputs_drained) = oneshot::channel();
    (
        CaptureEvents {
            physical_key: physical_key(),
            session: id.clone(),
            events,
            diverted: diverted_record(id, hook_maps),
            capture_plans,
            drained: Some(drained),
        },
        inputs_drained,
    )
}

fn app_plan(
    config: &openlogi_core::config::Config,
    app: &str,
    epoch: WheelProfileEpoch,
) -> DeviceCapturePlan {
    let mut plan = crate::capture_plan::plan_for_device(config, plan_device(), Some(app), 0, true);
    plan.dispatch.wheel_profile_epoch = epoch;
    plan
}

fn publish_app_plan(
    publication: &watch::Sender<Arc<CapturePublication>>,
    config: &openlogi_core::config::Config,
    app: &str,
    epoch: WheelProfileEpoch,
) {
    publication.send_replace(Arc::new(CapturePublication::from_plans(vec![app_plan(
        config, app, epoch,
    )])));
}

fn live_session_from_plan(epoch: u64, plan: DeviceCapturePlan) -> RunningSession {
    let (stop, _rx) = oneshot::channel();
    CaptureSession::active(session_id(epoch), plan.target, plan.dispatch, stop)
}

fn live_session_with_epoch(epoch: u64) -> RunningSession {
    live_session_from_plan(epoch, plan())
}

fn draining_session_with_epoch(epoch: u64) -> RunningSession {
    let mut session = live_session_with_epoch(epoch);
    assert_eq!(
        session.reconcile_with(None, stop_for_target_change),
        ReconcileAction::Retiring
    );
    session
}

#[tokio::test(start_paused = true)]
async fn planned_done_allows_an_immediate_successor_but_unexpected_done_is_paced() {
    let now = Instant::now();
    let mut planned = GestureSlot::running(draining_session_with_epoch(7));
    let (_, planned_unexpected) = planned
        .complete(&session_id(7), None, Some(now + RETRY_DELAY))
        .expect("the tracked planned completion should settle its session");
    assert!(
        !planned_unexpected
            && planned
                .recovery()
                .is_some_and(|recovery| recovery.restart_at.is_none()),
        "ordered Done after planned retirement must reconcile its successor immediately"
    );

    let mut unexpected = GestureSlot::running(live_session_with_epoch(8));
    let (_, failed) = unexpected
        .complete(&session_id(8), None, Some(now + RETRY_DELAY))
        .expect("the tracked unexpected completion should settle its session");
    assert!(failed);
    assert_eq!(
        unexpected
            .recovery()
            .and_then(|recovery| recovery.restart_at),
        Some(now + RETRY_DELAY)
    );
}

#[tokio::test(start_paused = true)]
async fn retry_deadline_is_not_postponed_by_ready_input() {
    let retry_at = Instant::now() + RETRY_DELAY;
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let waiter = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = wait_for_deadline(Some(retry_at)) => return Instant::now(),
                Some(()) = input_rx.recv() => {}
            }
        }
    });

    for _ in 0..10 {
        input_tx
            .send(())
            .expect("the deadline waiter should remain alive");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    tokio::task::yield_now().await;

    assert_eq!(
        waiter.await.expect("deadline waiter should finish"),
        retry_at,
        "recreating an absolute sleep after unrelated input must not postpone it"
    );
}

#[test]
fn suspended_device_io_disables_retry_deadlines() {
    let retry_at = Instant::now() + RETRY_DELAY;
    let slots = HashMap::from([(
        physical_key(),
        GestureSlot::recovering(None, Some(retry_at)),
    )]);

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, &slots),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(ReceiverRequestState::default(), false, &slots),
        None,
        "capture retries must stay dormant until visible resume",
    );
}

#[tokio::test(start_paused = true)]
async fn exclusive_receiver_access_disables_expired_recovery_deadlines() {
    let access = ReceiverAccess::default();
    let requests = access.subscribe_requests();
    let exclusive = access
        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
        .await;
    let restart_at = Instant::now();
    let slots = HashMap::from([(
        physical_key(),
        GestureSlot::recovering(None, Some(restart_at)),
    )]);

    assert_eq!(next_deadline(*requests.borrow(), true, &slots), None);
    drop(exclusive);
    assert_eq!(
        next_deadline(*requests.borrow(), true, &slots),
        Some(restart_at),
        "release makes the retry actionable without adding another backoff"
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_manager_waits_for_control_events_and_shutdown_between_retries() {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};
    use futures_lite::future::poll_once;

    for shutdown_requested in [false, true] {
        let (plans_tx, capture_plans) =
            watch::channel(Arc::new(CapturePublication::from_plans(vec![plan()])));
        let capture = CaptureChannelSlot::default();
        // A missing inventory channel makes the real session task fail without
        // opening hardware; ordered Done then drives normal restart recovery.
        let registry = openlogi_hid::ChannelRegistry::default();
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
        let mut scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
            false,
            VerticalScrollSensitivity::default(),
        )))
        .unwrap();
        let (shutdown_tx, shutdown) = oneshot::channel();
        let mut manager = std::pin::pin!(manage(GestureManagerContext {
            capture_plans,
            receiver_requests: access.subscribe_requests(),
            access: device_access,
            outputs: GestureOutputs::new(actions.dispatcher(), scroll.input(), Arc::default()),
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
        let before_event = Instant::now();
        if shutdown_requested {
            shutdown_tx.send(()).unwrap();
            assert!(matches!(manager.await, ManagerCompletion::Graceful));
        } else {
            drop(plans_tx);
            assert!(matches!(manager.await, ManagerCompletion::Unexpected));
        }
        assert_eq!(Instant::now(), before_event);
        scroll.shutdown();
        actions.shutdown();
    }
}

#[tokio::test]
async fn input_accepted_during_restoration_precedes_session_done() {
    let id = session_id(7);
    let key = physical_key();
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (listener_sink, inputs_drained) =
        capture_sink(&id, events.clone(), &SharedHookMaps::default());
    let (restored_tx, restored_rx) = oneshot::channel();
    let listener = tokio::spawn(async move {
        listener_sink.emit(CapturedInput::ButtonPulse(ButtonId::DpiToggle));
        let _ = restored_rx.await;
        None
    });
    let completion = tokio::spawn(complete_session(
        listener,
        inputs_drained,
        events,
        key.clone(),
        id.clone(),
        diverted_record(&id, &SharedHookMaps::default()),
    ));

    match event_rx.recv().await {
        Some(SessionEvent::Input(input)) => {
            assert_eq!(input.physical_key, key);
            assert_eq!(input.session, id);
            assert_eq!(input.input, CapturedInput::ButtonPulse(ButtonId::DpiToggle));
        }
        _ => panic!("captured input must be forwarded first"),
    }
    assert!(
        event_rx.try_recv().is_err(),
        "Done must wait while restoration still holds the listener"
    );

    restored_tx.send(()).expect("restore signal should be open");
    completion.await.expect("completion task should finish");
    match event_rx.recv().await {
        Some(SessionEvent::Done(done)) => assert_eq!(done.session, id),
        _ => panic!("Done must follow the last accepted input"),
    }
}

/// #2158: the session's own forwarder tells the hook which buttons it
/// diverted, before it forwards any press reported after that, and the hook
/// gets them back once the session's inputs are drained.
#[tokio::test]
async fn diverted_buttons_are_published_before_the_sessions_presses() {
    let id = session_id(7);
    let key = physical_key();
    let hook_maps = SharedHookMaps::default();
    // The OS input source of `route()`.
    let source = openlogi_hook::EventDevice {
        vendor_id: Some(0x046d),
        product_id: Some(0xc548),
        product_name: None,
    };
    let owned = |maps: &SharedHookMaps| {
        maps.read()
            .unwrap()
            .diverted_buttons
            .owns(&source, ButtonId::Back)
    };
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (sink, inputs_drained) = capture_sink(&id, events.clone(), &hook_maps);
    let diverted = diverted_record(&id, &hook_maps);
    sink.emit(CapturedInput::ButtonDiverted(ButtonId::Back));
    sink.emit(CapturedInput::ButtonPulse(ButtonId::Back));

    match event_rx.recv().await {
        Some(SessionEvent::Input(input)) => {
            assert_eq!(
                input.input,
                CapturedInput::ButtonPulse(ButtonId::Back),
                "the diversion report itself is not forwarded"
            );
            assert!(
                owned(&hook_maps),
                "the hook knows the owner before the manager sees the press"
            );
        }
        _ => panic!("the press must be forwarded"),
    }

    drop(sink);
    complete_session(
        tokio::spawn(async { None }),
        inputs_drained,
        events,
        key,
        id.clone(),
        diverted,
    )
    .await;
    match event_rx.recv().await {
        Some(SessionEvent::Done(done)) => assert_eq!(done.session, id),
        _ => panic!("Done must follow the last forwarded input"),
    }
    assert!(!owned(&hook_maps), "an ended session owns no button");
}

/// The failure behind #1857 and #1849: a session body that panics used to
/// take its completion report down with it, so the manager's slot stayed
/// `Running` and the device was never re-armed.
#[tokio::test]
async fn a_session_body_that_panics_still_reports_done_after_its_inputs() {
    let id = session_id(7);
    let key = physical_key();
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (sink, inputs_drained) = capture_sink(&id, events.clone(), &SharedHookMaps::default());
    let (never_restored, restore) = oneshot::channel::<Option<PendingCaptureRestore>>();
    let session_task = tokio::spawn(async move {
        sink.emit(CapturedInput::ButtonPulse(ButtonId::DpiToggle));
        drop(never_restored);
        // Dies with the sink still held, the way a panicking body does; the
        // unwind drops the source before completion is reported.
        restore.await.expect("simulated capture panic")
    });

    complete_session(
        session_task,
        inputs_drained,
        events,
        key.clone(),
        id.clone(),
        diverted_record(&id, &SharedHookMaps::default()),
    )
    .await;

    match event_rx.recv().await {
        Some(SessionEvent::Input(input)) => {
            assert_eq!(input.session, id);
            assert_eq!(input.input, CapturedInput::ButtonPulse(ButtonId::DpiToggle));
        }
        _ => panic!("the input accepted before the panic must still be forwarded"),
    }
    match event_rx.recv().await {
        Some(SessionEvent::Done(done)) => {
            assert_eq!(done.session, id);
            assert_eq!(done.physical_key, key);
            assert!(
                done.pending_restore.is_none(),
                "a panicked body has no restoration to hand over"
            );
        }
        _ => panic!("Done must follow the last input even when the body panicked"),
    }
}

#[tokio::test]
async fn session_done_waits_for_a_listener_already_copied_by_the_read_loop() {
    let id = session_id(7);
    let (events, mut queued) = mpsc::unbounded_channel();
    let (sink, inputs_drained) = capture_sink(&id, events.clone(), &SharedHookMaps::default());
    let listener = Arc::new(sink);
    let session_task = tokio::spawn(async { None });
    tokio::task::yield_now().await;
    assert!(session_task.is_finished());
    let mut completion = std::pin::pin!(complete_session(
        session_task,
        inputs_drained,
        events,
        physical_key(),
        id.clone(),
        diverted_record(&id, &SharedHookMaps::default())
    ));
    assert!(
        futures_lite::future::poll_once(&mut completion)
            .await
            .is_none(),
        "a completed task still waits for its last read-loop listener"
    );
    listener.emit(CapturedInput::ButtonUp(ButtonId::GestureButton));
    drop(listener);
    completion.await;
    assert!(matches!(
        queued.try_recv().unwrap(),
        SessionEvent::Input(CapturedEvent {
            input: CapturedInput::ButtonUp(ButtonId::GestureButton),
            ..
        })
    ));
    assert!(matches!(queued.try_recv().unwrap(), SessionEvent::Done(_)));
}

#[test]
fn accepts_inputs_from_the_current_session_until_teardown_finishes() {
    assert!(dispatch_context_for(&session_id(7), Some(&live_session_with_epoch(7))).is_some());
    assert!(
        dispatch_context_for(&session_id(6), Some(&live_session_with_epoch(7))).is_none(),
        "a superseded session's queued input is stale"
    );
    assert!(
        dispatch_context_for(&session_id(7), Some(&draining_session_with_epoch(7))).is_some(),
        "a draining session still owns diverted input until restoration completes"
    );
    assert!(dispatch_context_for(&session_id(7), None).is_none());
}

#[tokio::test]
async fn exclusive_request_retires_capture_without_rejecting_owned_input() {
    let access = ReceiverAccess::default();
    let session_lease = access
        .try_acquire_for_session()
        .expect("capture should acquire the receiver before pairing");
    let pairing = tokio::spawn({
        let access = access.clone();
        async move {
            access
                .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !access.exclusive_requested() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pairing should announce its exclusive request");

    let (plans, _) =
        tokio::sync::watch::channel(Arc::new(CapturePublication::from_plans(vec![plan()])));
    let plans = plans.subscribe();
    let requests = access.subscribe_requests();
    assert!(wanted_sessions(*requests.borrow(), &plans).is_empty());

    let mut session = live_session_with_epoch(7);
    assert_eq!(
        session.reconcile_with(None, stop_for_target_change),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());
    assert!(
        dispatch_context_for(&session_id(7), Some(&session)).is_some(),
        "an exclusive request retires capture but must not reject input while firmware remains diverted"
    );

    pairing.abort();
    let _ = pairing.await;
    drop(session_lease);
}

#[test]
fn an_active_session_refreshes_bindings_without_rearming_hardware() {
    let mut old_plan = plan();
    old_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::MissionControl)].into(),
    );
    old_plan
        .target
        .spec
        .divert_gesture_buttons
        .push((0x0056, ButtonId::Forward));
    let mut session = live_session_from_plan(7, old_plan.clone());

    let mut new_plan = old_plan;
    new_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::ShowDesktop)].into(),
    );
    assert_eq!(session.target(), &new_plan.target);

    assert_eq!(
        session.reconcile_with(
            Some((&new_plan.target, &new_plan.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "a hot plan refresh must cancel input lifecycles admitted under the old action map"
    );

    assert!(session.is_active());
    assert_eq!(
        session.dispatch().side_gesture_bindings[&ButtonId::Forward][&GestureDirection::Click],
        Action::ShowDesktop,
        "an unchanged capture target must still adopt the new app/profile action"
    );
}

#[test]
fn side_gesture_transition_keeps_the_retiring_plan_until_native_restore() {
    let mut old_plan = plan();
    old_plan.dispatch.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::MissionControl)].into(),
    );
    old_plan
        .target
        .spec
        .divert_gesture_buttons
        .push((0x0056, ButtonId::Forward));
    let mut session = live_session_from_plan(7, old_plan.clone());

    let mut published_without_hook = old_plan;
    published_without_hook
        .dispatch
        .side_gesture_bindings
        .clear();
    published_without_hook
        .target
        .spec
        .divert_gesture_buttons
        .clear();
    assert_ne!(session.target(), &published_without_hook.target);
    assert_eq!(
        session.reconcile_with(
            Some((
                &published_without_hook.target,
                &published_without_hook.dispatch,
            )),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());

    let (_, retained) = dispatch_context_for(session.id(), Some(&session))
        .expect("the draining session must remain an admissible input owner");
    assert!(
        retained
            .side_gesture_bindings
            .contains_key(&ButtonId::Forward)
    );
    assert!(
        session
            .target()
            .spec
            .divert_gesture_buttons
            .iter()
            .any(|&(_, button)| button == ButtonId::Forward),
        "the retiring target must stay frozen while firmware diversion remains active"
    );
}

#[test]
fn capture_target_changes_schedule_the_old_session_for_retirement() {
    let session = live_session_with_epoch(7);
    let mut plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        crate::capture_plan::PlanDevice {
            capabilities: Some(openlogi_core::device::Capabilities::from_feature_ids(&[
                0x2150,
            ])),
            physical_key: physical_key(),
            config_key: "mouse-a",
            route: session.target().route.clone(),
            layout: None,
        },
        None,
        0,
        true,
    );
    assert_eq!(session.target(), &plan.target);

    plan.target.rearm_generation = 1;
    assert_ne!(
        session.target(),
        &plan.target,
        "a capture-target epoch change must retire the old session"
    );
}

#[test]
fn receiver_route_change_requests_a_firmware_handoff() {
    let old_plan = plan();
    let mut new_plan = old_plan.clone();
    let successor_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    new_plan.target.route.clone_from(&successor_route);
    let (stop, mut stopped) = oneshot::channel();
    let mut session =
        CaptureSession::active(session_id(7), old_plan.target, old_plan.dispatch, stop);

    assert_eq!(
        session.reconcile_with(
            Some((&new_plan.target, &new_plan.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert_eq!(
        stopped
            .try_recv()
            .expect("route change should stop the active session"),
        CaptureSessionStop::Handoff(successor_route),
        "teardown must restore through the receiver the mouse moved to"
    );
}

#[test]
fn same_route_rearm_shuts_down_instead_of_handing_off() {
    let old_plan = plan();
    let mut rearmed = old_plan.clone();
    rearmed.target.rearm_generation = old_plan.target.rearm_generation + 1;
    let (stop, mut stopped) = oneshot::channel();
    let mut session =
        CaptureSession::active(session_id(7), old_plan.target, old_plan.dispatch, stop);

    assert_eq!(
        session.reconcile_with(
            Some((&rearmed.target, &rearmed.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::Retiring
    );
    assert_eq!(
        stopped
            .try_recv()
            .expect("a reconnect rearm should stop the session"),
        CaptureSessionStop::Shutdown,
        "the same route restores through its current publication"
    );
}

#[test]
fn config_key_adoption_hot_refreshes_the_same_physical_capture_slot() {
    let old_plan = plan();
    let physical_key = old_plan.target.physical_key.clone();
    let mut adopted_plan = old_plan.clone();
    adopted_plan.dispatch.config_key = "unit:00000001".to_owned();
    assert_eq!(old_plan.target, adopted_plan.target);

    let mut sessions = HashMap::from([(physical_key.clone(), live_session_from_plan(7, old_plan))]);
    let wanted = HashMap::from([(physical_key.clone(), adopted_plan)]);
    let running = sessions
        .get_mut(&physical_key)
        .expect("the physical slot should already be occupied");
    let desired = wanted
        .get(&physical_key)
        .map(|plan| (&plan.target, &plan.dispatch));

    assert_eq!(
        running.reconcile_with(desired, stop_for_target_change),
        ReconcileAction::DispatchChanged
    );
    running.rekey(&wanted[&physical_key].dispatch.config_key);
    assert!(running.is_active());
    assert_eq!(running.id().device_key(), "unit:00000001");
    assert!(
        sessions.contains_key(&physical_key),
        "the old session must keep the one physical slot until ordered Done"
    );
}

#[test]
fn active_session_adopts_action_only_plan_changes_without_rearming() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Copy),
    );
    let first = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Paste),
    );
    let rebound = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    assert_eq!(first.target, rebound.target);
    assert_eq!(
        session.reconcile_with(
            Some((&rebound.target, &rebound.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session.dispatch().bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn active_session_adopts_gesture_and_per_app_dispatch_changes() {
    let mut config = openlogi_core::config::Config::default();
    config.set_gesture_mode("mouse-a", ButtonId::GestureButton, true);
    let first = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    let mut session = live_session_from_plan(7, first.clone());

    config.set_gesture_direction(
        "mouse-a",
        ButtonId::GestureButton,
        GestureDirection::Right,
        Action::MissionControl,
    );
    let gestured = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    assert_eq!(first.target, gestured.target);
    assert_eq!(
        session.reconcile_with(
            Some((&gestured.target, &gestured.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session
            .dispatch()
            .gesture_bindings
            .get(&ButtonId::GestureButton)
            .and_then(|map| map.get(&GestureDirection::Right)),
        Some(&Action::MissionControl)
    );

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Copy),
    );
    let base = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    let mut session = live_session_from_plan(8, base.clone());
    config.set_per_app_binding(
        "mouse-a",
        "com.example.Editor",
        ButtonId::DpiToggle,
        Some(Action::Paste),
    );
    let per_app = crate::capture_plan::plan_for_device(
        &config,
        plan_device(),
        Some("com.example.Editor"),
        0,
        true,
    );
    assert_eq!(base.target, per_app.target);
    assert_eq!(
        session.reconcile_with(
            Some((&per_app.target, &per_app.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session.dispatch().bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn wheel_configuration_changes_refresh_without_rearming_hardware() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::NextTab),
    );
    let first = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::VolumeUp),
    );
    let rebound = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    assert_eq!(
        first.target, rebound.target,
        "both custom bindings require the same HID++ diversion"
    );
    assert_eq!(
        session.reconcile_with(
            Some((&rebound.target, &rebound.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "dispatch-only binding changes must not cycle firmware diversion"
    );
    assert!(session.is_active());

    config.set_device_thumbwheel_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MIN));
    let rescaled = crate::capture_plan::plan_for_device(&config, plan_device(), None, 0, true);
    assert_eq!(rebound.target, rescaled.target);
    assert_eq!(
        session.reconcile_with(
            Some((&rescaled.target, &rescaled.dispatch)),
            stop_for_target_change,
        ),
        ReconcileAction::DispatchChanged,
        "an already-diverted wheel needs a state reset, not a hardware restart"
    );
    assert!(session.is_active());
}

#[tokio::test]
async fn pending_restore_follows_a_disabled_devices_latest_route() {
    use std::collections::BTreeMap;

    let stale = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let latest = DeviceRoute::Bolt {
        receiver_uid: "receiver-c".to_owned(),
        slot: 1,
    };
    let registry = openlogi_hid::ChannelRegistry::default();
    let key = physical_key();
    let recovering = |route: DeviceRoute| {
        HashMap::from([(
            key.clone(),
            GestureSlot::recovering(
                Some(PendingRestore {
                    token: PendingCaptureRestore::detached(route),
                    retry_at: Instant::now(),
                }),
                None,
            ),
        )])
    };
    let route_of = |slots: &HashMap<PhysicalDeviceKey, GestureSlot>| {
        slots
            .get(&key)
            .and_then(GestureSlot::recovery)
            .and_then(|recovery| recovery.pending_restore.as_ref())
            .map(|pending| pending.token.route().clone())
    };

    let mut stuck = recovering(stale.clone());
    retry_pending_restores(&mut stuck, &registry, None, Instant::now()).await;
    assert_eq!(
        route_of(&stuck),
        Some(stale.clone()),
        "without a published route the owed restore stays on its last target"
    );

    let mut followed = recovering(stale);
    let publication =
        CapturePublication::new(Vec::new(), BTreeMap::from([(key.clone(), latest.clone())]));
    retry_pending_restores(&mut followed, &registry, Some(&publication), Instant::now()).await;
    assert!(
        publication.plans().is_empty(),
        "capture stays disarmed while the device is disabled"
    );
    assert_eq!(
        route_of(&followed),
        Some(latest),
        "restore must follow the mouse onto the receiver inventory names now"
    );
}

fn dispatch_fixture() -> (
    crate::runtime::ActionRuntime,
    crate::runtime::scroll::ScrollRuntime,
    InputDispatcher,
    mpsc::UnboundedReceiver<crate::runtime::ActionRequest>,
) {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};

    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (ring, requests) = mpsc::unbounded_channel();
    let device_access = DeviceAccess {
        channel: CaptureChannelSlot::default(),
        registry: openlogi_hid::ChannelRegistry::default(),
        receiver_access: ReceiverAccess::default(),
        device_io,
        transport_turns: crate::hardware::TransportTurns::default(),
    };
    let actions =
        crate::runtime::ActionRuntime::new(Arc::default(), Arc::default(), device_access, ring)
            .unwrap();
    let scroll = ScrollRuntime::spawn(Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::default(),
    )))
    .unwrap();
    let dispatcher = InputDispatcher::new(GestureOutputs::new(
        actions.dispatcher(),
        scroll.input(),
        Arc::default(),
    ));
    (actions, scroll, dispatcher, requests)
}

#[tokio::test]
async fn a_pointer_only_plan_refresh_keeps_a_held_gesture() {
    let (mut actions, mut scroll, mut dispatcher, _requests) = dispatch_fixture();

    let mut held = plan();
    held.dispatch.gesture_bindings.insert(
        ButtonId::GestureButton,
        [(GestureDirection::Left, Action::PreviousDesktop)].into(),
    );
    let mut session = live_session_from_plan(7, held.clone());
    let id = session.id().clone();
    dispatcher.dispatch(
        &id,
        session.dispatch(),
        CapturedInput::ButtonDown(ButtonId::GestureButton),
    );
    assert!(dispatcher.holds_gesture_press(&id, ButtonId::GestureButton));

    // The swipe switched desktops, leaving the pointer over another window
    // (#1634): the republished plan differs only in its pointer target.
    let mut moved = held;
    moved.dispatch.pointer = Some(crate::runtime::PointerSelection::new(
        openlogi_hook::PointerTarget::Desktop,
        std::collections::BTreeSet::new(),
    ));
    reconcile_session(
        &mut session,
        Some((&moved.target, &moved.dispatch)),
        &mut dispatcher,
    );
    assert!(
        dispatcher.holds_gesture_press(&id, ButtonId::GestureButton),
        "a pointer-only refresh must not end the hold that caused it"
    );

    // A real binding change still ends every admitted press.
    let mut rebound = moved;
    rebound.dispatch.gesture_bindings.insert(
        ButtonId::GestureButton,
        [(GestureDirection::Left, Action::Copy)].into(),
    );
    reconcile_session(
        &mut session,
        Some((&rebound.target, &rebound.dispatch)),
        &mut dispatcher,
    );
    assert!(!dispatcher.holds_gesture_press(&id, ButtonId::GestureButton));

    scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
async fn thumbwheel_rearming_waits_for_independent_accepted_buttons_to_finish() {
    use openlogi_core::binding::LongPressBinding;
    for starting_captured in [false, true] {
        for button in [ButtonId::GestureButton, ButtonId::Back] {
            let (mut actions, mut scroll, mut dispatcher, _requests) = dispatch_fixture();
            let mut held = plan();
            held.target.spec.capture_thumbwheel = starting_captured;
            held.dispatch.thumbwheel_sensitivity = if starting_captured {
                ThumbwheelSensitivity::from_rounded(28.0)
            } else {
                ThumbwheelSensitivity::DEFAULT
            };
            if button == ButtonId::GestureButton {
                held.dispatch.gesture_bindings.insert(
                    button,
                    [(GestureDirection::Left, Action::PreviousDesktop)].into(),
                );
            } else {
                held.dispatch.bindings.insert(
                    button,
                    Binding::LongPress(LongPressBinding::new(
                        Action::None,
                        Action::ShowActionsRing,
                    )),
                );
            }
            let mut session = live_session_from_plan(7, held.clone());
            let id = session.id().clone();
            dispatcher.dispatch(&id, session.dispatch(), CapturedInput::ButtonDown(button));
            assert!(dispatcher.holds_input(&id));
            let mut switched = held;
            switched.target.spec.capture_thumbwheel = !starting_captured;
            switched.dispatch.thumbwheel_sensitivity = if starting_captured {
                ThumbwheelSensitivity::DEFAULT
            } else {
                ThumbwheelSensitivity::from_rounded(28.0)
            };
            switched.dispatch.thumbwheel_app = Some("other".into());
            reconcile_session(
                &mut session,
                Some((&switched.target, &switched.dispatch)),
                &mut dispatcher,
            );
            assert!(
                session.is_active(),
                "a wheel-only hardware change must wait for the admitted {button}"
            );
            assert!(dispatcher.holds_input(&id));
            assert_eq!(
                session.dispatch().thumbwheel_sensitivity,
                switched.dispatch.thumbwheel_sensitivity,
                "new App scaling applies while hardware rearming waits"
            );
            dispatcher.dispatch(&id, session.dispatch(), CapturedInput::ButtonUp(button));
            assert!(!dispatcher.holds_input(&id));
            reconcile_session(
                &mut session,
                Some((&switched.target, &switched.dispatch)),
                &mut dispatcher,
            );
            assert!(
                !session.is_active(),
                "release lets normal restore/rearm finish the native/capture transition"
            );
            scroll.shutdown();
            actions.shutdown();
        }
    }
}

#[tokio::test]
async fn captured_thumbwheel_uses_the_app_scale_in_the_real_scroll_worker() {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};
    let (mut actions, mut unused_scroll, _, _requests) = dispatch_fixture();
    let preferences = Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::DEFAULT,
    ));
    let (frames, received) = std::sync::mpsc::channel();
    let mut scroll = ScrollRuntime::spawn_observed(
        Arc::clone(&preferences),
        move |delta, phase| {
            frames.send((delta, phase)).unwrap();
        },
        |_| panic!("a captured horizontal tick must retain scroll phases"),
    )
    .unwrap();
    let mut config = openlogi_core::config::Config::default();
    config.set_app_thumbwheel_sensitivity("editor", ThumbwheelSensitivity::from_rounded(28.0));
    let captured = app_plan(&config, "editor", preferences.wheel_profile_epoch());
    assert!(captured.target.spec.capture_thumbwheel);
    let mut dispatcher = InputDispatcher::new(GestureOutputs::new(
        actions.dispatcher(),
        scroll.input(),
        Arc::default(),
    ));
    dispatcher.dispatch(
        &session_id(7),
        &captured.dispatch,
        CapturedInput::Scroll {
            increments: 1,
            resolution: openlogi_hid::thumbwheel::WheelResolution::UNKNOWN,
        },
    );
    let (delta, phase) = received.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(delta, ScrollDelta::wheel_ticks(-2.0, 0.0));
    assert_eq!(phase, openlogi_inject::SmoothScrollPhase::Began);
    scroll.shutdown();
    unused_scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
async fn draining_thumbwheel_reports_use_the_new_native_app_policy() {
    use crate::runtime::scroll::{ScrollPreferences, ScrollRuntime};
    let (mut actions, mut unused_scroll, _, _requests) = dispatch_fixture();
    let preferences = Arc::new(ScrollPreferences::new(
        false,
        VerticalScrollSensitivity::DEFAULT,
    ));
    let (frames, received) = std::sync::mpsc::channel();
    let mut scroll = ScrollRuntime::spawn_observed(
        Arc::clone(&preferences),
        move |delta, _| {
            frames.send(delta).unwrap();
        },
        |_| panic!("thumb output must retain phases"),
    )
    .unwrap();
    let mut config = openlogi_core::config::Config::default();
    config.set_app_thumbwheel_sensitivity("captured", ThumbwheelSensitivity::from_rounded(28.0));
    config.set_app_thumbwheel_sensitivity("native", ThumbwheelSensitivity::DEFAULT);
    let first = app_plan(&config, "captured", preferences.wheel_profile_epoch());
    let next = app_plan(&config, "native", preferences.wheel_profile_epoch());
    assert!(first.target.spec.capture_thumbwheel && !next.target.spec.capture_thumbwheel);
    let (published, plans) = watch::channel(Arc::new(CapturePublication::from_plans(vec![
        first.clone(),
    ])));
    let (events, mut queued) = mpsc::unbounded_channel();
    let sink = CaptureEvents {
        physical_key: physical_key(),
        session: session_id(7),
        events,
        diverted: diverted_record(&session_id(7), &SharedHookMaps::default()),
        capture_plans: plans.clone(),
        drained: None,
    };
    let mut manager = GestureManagerState {
        slots: HashMap::from([(
            physical_key(),
            GestureSlot::running(live_session_from_plan(7, first)),
        )]),
        input_dispatcher: InputDispatcher::new(GestureOutputs::new(
            actions.dispatcher(),
            scroll.input(),
            Arc::default(),
        )),
        lease: std::sync::Weak::new(),
    };
    let access = ReceiverAccess::default();
    let requests = access.subscribe_requests();
    let report = || CapturedInput::Scroll {
        increments: 1,
        resolution: openlogi_hid::thumbwheel::WheelResolution::UNKNOWN,
    };
    sink.emit(report());
    published.send_replace(Arc::new(CapturePublication::from_plans(vec![next])));
    manager.handle_session_event(queued.try_recv().unwrap(), true, &requests, &plans);
    assert!(
        received.try_recv().is_err(),
        "the old App's queued tick is discarded"
    );
    assert!(
        !manager.slots[&physical_key()]
            .session()
            .unwrap()
            .is_active()
    );
    sink.emit(report());
    manager.handle_session_event(queued.try_recv().unwrap(), true, &requests, &plans);
    let delta = received.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        delta,
        ScrollDelta::wheel_ticks(-1.0, 0.0),
        "new reports during restoration use the new App's 14, not the frozen old 28"
    );
    scroll.shutdown();
    unused_scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
async fn thumbwheel_notifications_and_action_thresholds_follow_exact_app_epochs() {
    let (mut actions, mut scroll, _, mut requests) = dispatch_fixture();
    let mut config = openlogi_core::config::Config::default();
    for app in ["a", "b"] {
        config.set_app_thumbwheel_sensitivity(app, ThumbwheelSensitivity::DEFAULT);
    }
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::ShowActionsRing),
    );
    let mut orch = crate::orchestrator::Orchestrator::new(
        config.clone(),
        Arc::new(crate::observable::ObservableState::new("test".into())),
    );
    let preferences = orch.shared().scroll_preferences;
    let mut profile_scroll =
        crate::runtime::scroll::ScrollRuntime::spawn(Arc::clone(&preferences)).unwrap();
    let dispatcher = InputDispatcher::new(GestureOutputs::new(
        actions.dispatcher(),
        profile_scroll.input(),
        Arc::default(),
    ));
    orch.set_current_app(Some(openlogi_core::app::ForegroundApp::unnamed("a".into())));
    let first = app_plan(&config, "a", preferences.wheel_profile_epoch());
    let (published, plans) = watch::channel(Arc::new(CapturePublication::from_plans(vec![
        first.clone(),
    ])));
    let (events, mut queued) = mpsc::unbounded_channel();
    let sink = CaptureEvents {
        physical_key: physical_key(),
        session: session_id(7),
        events,
        diverted: diverted_record(&session_id(7), &SharedHookMaps::default()),
        capture_plans: plans.clone(),
        drained: None,
    };
    let mut manager = GestureManagerState {
        slots: HashMap::from([(
            physical_key(),
            GestureSlot::running(live_session_from_plan(7, first)),
        )]),
        input_dispatcher: dispatcher,
        lease: std::sync::Weak::new(),
    };
    let access = ReceiverAccess::default();
    let receiver_requests = access.subscribe_requests();
    let report = |increments| CapturedInput::Scroll {
        increments,
        resolution: openlogi_hid::thumbwheel::WheelResolution::UNKNOWN,
    };
    // No forwarding task is polled: the first queue already retains A's owner.
    sink.emit(report(14));
    orch.set_current_app(Some(openlogi_core::app::ForegroundApp::unnamed("b".into())));
    let next = app_plan(&config, "b", preferences.wheel_profile_epoch());
    published.send_replace(Arc::new(CapturePublication::from_plans(vec![next])));
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    assert!(
        requests.try_recv().is_err(),
        "A's queued action must not fire in B"
    );
    sink.emit(report(13));
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    requests.try_recv().unwrap_err();
    for app in ["a", "b"] {
        orch.set_current_app(Some(openlogi_core::app::ForegroundApp::unnamed(app.into())));
    }
    let returned = app_plan(&config, "b", preferences.wheel_profile_epoch());
    published.send_replace(Arc::new(CapturePublication::from_plans(vec![returned])));
    sink.emit(report(1));
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    assert!(
        requests.try_recv().is_err(),
        "coalesced B→A→B must clear B's old partial threshold"
    );
    sink.emit(report(13));
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    assert!(matches!(
        requests.try_recv().unwrap(),
        crate::runtime::ActionRequest::ShowActionsRing { .. }
    ));
    profile_scroll.shutdown();
    scroll.shutdown();
    actions.shutdown();
}

#[tokio::test]
async fn buffered_thumbwheel_reports_are_not_reinterpreted_as_a_new_binding() {
    let (mut actions, mut scroll, dispatcher, mut requests) = dispatch_fixture();
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollDown,
        Binding::Single(Action::ShowActionsRing),
    );
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::None),
    );
    let epoch = WheelProfileEpoch::default();
    let first = app_plan(&config, "b", epoch);
    assert!(
        first.target.spec.capture_thumbwheel,
        "the opposite custom rotation keeps reports diverted"
    );
    let (published, plans) = watch::channel(Arc::new(CapturePublication::from_plans(vec![
        first.clone(),
    ])));
    let (events, mut queued) = mpsc::unbounded_channel();
    let sink = CaptureEvents {
        physical_key: physical_key(),
        session: session_id(7),
        events,
        diverted: diverted_record(&session_id(7), &SharedHookMaps::default()),
        capture_plans: plans.clone(),
        drained: None,
    };
    let mut manager = GestureManagerState {
        slots: HashMap::from([(
            physical_key(),
            GestureSlot::running(live_session_from_plan(7, first)),
        )]),
        input_dispatcher: dispatcher,
        lease: std::sync::Weak::new(),
    };
    let access = ReceiverAccess::default();
    let receiver_requests = access.subscribe_requests();
    let report = || CapturedInput::Scroll {
        increments: 14,
        resolution: openlogi_hid::thumbwheel::WheelResolution::UNKNOWN,
    };
    sink.emit(report());
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::ShowActionsRing),
    );
    publish_app_plan(&published, &config, "b", epoch);
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    assert!(
        requests.try_recv().is_err(),
        "a buffered suppressed report must not be reinterpreted as the new action"
    );
    sink.emit(report());
    manager.handle_session_event(queued.try_recv().unwrap(), true, &receiver_requests, &plans);
    assert!(matches!(
        requests.try_recv().unwrap(),
        crate::runtime::ActionRequest::ShowActionsRing { .. }
    ));
    scroll.shutdown();
    actions.shutdown();
}

#[test]
fn withdrawing_capture_uses_the_sessions_last_hot_wheel_dispatch() {
    let config = openlogi_core::config::Config::default();
    let mut original = app_plan(&config, "a", WheelProfileEpoch::default());
    original.dispatch.thumbwheel_sensitivity = ThumbwheelSensitivity::from_rounded(28.0);
    let mut session = live_session_from_plan(7, original.clone());
    let mut updated = original;
    updated.dispatch.thumbwheel_sensitivity = ThumbwheelSensitivity::from_rounded(42.0);
    assert_eq!(
        session.reconcile_with(
            Some((&updated.target, &updated.dispatch)),
            stop_for_target_change
        ),
        ReconcileAction::DispatchChanged
    );
    assert_eq!(
        session.reconcile_with(None, stop_for_target_change),
        ReconcileAction::Retiring
    );
    assert!(
        WheelNotification::for_plan(&updated.dispatch)
            .resolve(session.dispatch(), None, WheelProfileEpoch::default())
            .is_some(),
        "a matching report accepted just before withdrawal remains owned during restoration"
    );
    let selected = WheelNotification::Retiring
        .resolve(session.dispatch(), None, WheelProfileEpoch::default())
        .unwrap();
    assert_eq!(
        selected.thumbwheel_sensitivity,
        ThumbwheelSensitivity::from_rounded(42.0)
    );
    assert!(
        WheelNotification::Retiring
            .resolve(
                session.dispatch(),
                Some(&updated.dispatch),
                WheelProfileEpoch::default()
            )
            .is_none(),
        "a withdrawn report never acquires a new publication"
    );
}

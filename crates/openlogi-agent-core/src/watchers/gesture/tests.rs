use super::*;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::DeviceRoute;
use openlogi_hid::session::gesture::{RawModeJournal, TouchpadJournalError, TouchpadJournalStore};

struct StubJournal(Option<RawModeJournal>);

impl TouchpadJournalStore for StubJournal {
    fn load(&self, _: &str) -> Result<Option<RawModeJournal>, TouchpadJournalError> {
        Ok(self.0)
    }

    fn save(&self, _: &str, _: RawModeJournal) -> Result<(), TouchpadJournalError> {
        Ok(())
    }

    fn clear(&self, _: &str) -> Result<(), TouchpadJournalError> {
        Ok(())
    }
}

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

fn plan() -> DeviceCapturePlan {
    crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    )
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
    assert_eq!(session.reconcile(None), ReconcileAction::Retiring);
    session
}

#[tokio::test(start_paused = true)]
async fn planned_done_allows_an_immediate_successor_but_unexpected_done_is_paced() {
    let planned = draining_session_with_epoch(7);
    let CompletionAction::Remove {
        unexpected: planned_unexpected,
    } = planned.completion(&session_id(7))
    else {
        panic!("the tracked planned completion should remove its session");
    };
    assert!(
        restart_deadline(planned_unexpected, Instant::now()).is_none(),
        "ordered Done after planned retirement must reconcile its successor immediately"
    );

    let unexpected = live_session_with_epoch(8);
    let CompletionAction::Remove { unexpected: failed } = unexpected.completion(&session_id(8))
    else {
        panic!("the tracked unexpected completion should remove its session");
    };
    let retry_at =
        restart_deadline(failed, Instant::now()).expect("an unexpected completion must be paced");
    assert_eq!(retry_at, Instant::now() + RETRY_DELAY);
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
    let restart_after = HashMap::from([(physical_key(), retry_at)]);

    assert_eq!(
        next_deadline(
            ReceiverRequestState::default(),
            true,
            &HashMap::new(),
            &restart_after,
        ),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(
            ReceiverRequestState::default(),
            false,
            &HashMap::new(),
            &restart_after,
        ),
        None,
        "capture retries must stay dormant until visible resume",
    );
}

#[tokio::test]
async fn input_accepted_during_restoration_precedes_session_done() {
    let id = session_id(7);
    let key = physical_key();
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let (session_tx, session_rx) = mpsc::unbounded_channel();
    let listener_sink = session_tx.clone();
    drop(session_tx);
    let forward_task = spawn_input_forwarder(key.clone(), id.clone(), session_rx, events.clone());
    let completion = tokio::spawn(report_done_after_inputs(
        forward_task,
        events,
        SessionDone {
            physical_key: key.clone(),
            session: id.clone(),
            pending_restore: None,
            error: None,
        },
    ));
    let (restored_tx, restored_rx) = oneshot::channel();
    let listener = tokio::spawn(async move {
        listener_sink
            .send(CapturedInput::ButtonPulse(ButtonId::DpiToggle))
            .expect("the capture forwarder should remain open");
        let _ = restored_rx.await;
        drop(listener_sink);
    });

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
    listener.await.expect("listener task should finish");
    completion.await.expect("completion task should finish");
    match event_rx.recv().await {
        Some(SessionEvent::Done(done)) => assert_eq!(done.session, id),
        _ => panic!("Done must follow the last accepted input"),
    }
}

#[test]
fn shutdown_requests_every_live_session_stop() {
    let first_plan = plan();
    let first_key = first_plan.target.physical_key.clone();
    let (first_stop, mut first_stopped) = oneshot::channel();
    let first = CaptureSession::active(
        session_id(7),
        first_plan.target,
        first_plan.dispatch,
        first_stop,
    );
    let mut second_plan = plan();
    second_plan.target.physical_key =
        PhysicalDeviceKey::parse("unit:00000002").expect("fixture should be a physical key");
    let second_key = second_plan.target.physical_key.clone();
    let (second_stop, mut second_stopped) = oneshot::channel();
    let second = CaptureSession::active(
        session_id(8),
        second_plan.target,
        second_plan.dispatch,
        second_stop,
    );
    let mut sessions = HashMap::from([(first_key, first), (second_key, second)]);
    let mut cancelled = Vec::new();

    request_session_stops(&mut sessions, |session| cancelled.push(session.clone()));

    cancelled.sort_by_key(HidppSessionId::epoch);
    assert_eq!(cancelled, vec![session_id(7), session_id(8)]);
    assert_eq!(first_stopped.try_recv(), Ok(()));
    assert_eq!(second_stopped.try_recv(), Ok(()));
    assert!(sessions.values().all(|session| !session.is_active()));
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

    let plans = Arc::new(vec![plan()]);
    let requests = access.subscribe_requests();
    let monitor = Arc::new(crate::touchpad_monitor::TouchpadMonitor::default());
    assert!(wanted_sessions(*requests.borrow(), &plans, &monitor, None).is_empty());

    let mut session = live_session_with_epoch(7);
    assert_eq!(session.reconcile(None), ReconcileAction::Retiring);
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
        session.reconcile(Some((&new_plan.target, &new_plan.dispatch))),
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
        session.reconcile(Some((
            &published_without_hook.target,
            &published_without_hook.dispatch,
        ))),
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
        physical_key(),
        "mouse-a",
        session.target().route.clone(),
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

    assert_eq!(running.reconcile(desired), ReconcileAction::DispatchChanged);
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
    let first = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Paste),
    );
    let rebound = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    assert_eq!(first.target, rebound.target);
    assert_eq!(
        session.reconcile(Some((&rebound.target, &rebound.dispatch))),
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
    let first = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_gesture_direction(
        "mouse-a",
        ButtonId::GestureButton,
        GestureDirection::Right,
        Action::MissionControl,
    );
    let gestured = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    assert_eq!(first.target, gestured.target);
    assert_eq!(
        session.reconcile(Some((&gestured.target, &gestured.dispatch))),
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
    let base = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    let mut session = live_session_from_plan(8, base.clone());
    config.set_per_app_binding(
        "mouse-a",
        "com.example.Editor",
        ButtonId::DpiToggle,
        Some(Action::Paste),
    );
    let per_app = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        Some("com.example.Editor"),
        0,
        true,
    );
    assert_eq!(base.target, per_app.target);
    assert_eq!(
        session.reconcile(Some((&per_app.target, &per_app.dispatch))),
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
    let first = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    let mut session = live_session_from_plan(7, first.clone());

    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::VolumeUp),
    );
    let rebound = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    assert_eq!(
        first.target, rebound.target,
        "both custom bindings require the same HID++ diversion"
    );
    assert_eq!(
        session.reconcile(Some((&rebound.target, &rebound.dispatch))),
        ReconcileAction::DispatchChanged,
        "dispatch-only binding changes must not cycle firmware diversion"
    );
    assert!(session.is_active());

    config.set_device_thumbwheel_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MIN));
    let rescaled = crate::capture_plan::plan_for_device(
        &config,
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    assert_eq!(rebound.target, rescaled.target);
    assert_eq!(
        session.reconcile(Some((&rescaled.target, &rescaled.dispatch))),
        ReconcileAction::DispatchChanged,
        "an already-diverted wheel needs a state reset, not a hardware restart"
    );
    assert!(session.is_active());
}

#[test]
fn diagnostics_temporarily_arm_only_raw_touchpad_plans() {
    let touchpad = crate::capture_plan::plan_for_device_with_touchpad(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "touchpad-a",
        route(),
        None,
        crate::capture_plan::CapturePlanOptions {
            touchpad_journal_id: Some("unit:01020304".into()),
            rearm_generation: 0,
            os_mouse_hook_available: true,
        },
    );
    let mouse = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    let monitor = Arc::new(crate::touchpad_monitor::TouchpadMonitor::default());
    let journal = StubJournal(None);

    assert!(!touchpad.target.spec.capture_touchpad);
    monitor.poll("touchpad-a");
    assert!(
        effective_plan(&touchpad, &monitor, Some(&journal))
            .expect("diagnostic touchpad plan")
            .target
            .spec
            .capture_touchpad
    );
    assert!(
        !effective_plan(&mouse, &monitor, Some(&journal))
            .expect("ordinary mouse plan")
            .target
            .spec
            .capture_touchpad
    );
}

#[test]
fn recovery_session_is_wanted_only_while_its_journal_exists() {
    let plan = crate::capture_plan::touchpad_recovery_plan(
        physical_key(),
        "touchpad-a",
        route(),
        "unit:01020304".to_string(),
        0,
    );
    let plans = Arc::new(vec![plan]);
    let monitor = Arc::new(crate::touchpad_monitor::TouchpadMonitor::default());
    let missing = StubJournal(None);
    let existing = StubJournal(Some(RawModeJournal {
        original: 0,
        requested: 5,
        readback: Some(5),
        armed: true,
    }));

    assert!(
        wanted_sessions(
            ReceiverRequestState::default(),
            &plans,
            &monitor,
            Some(&missing),
        )
        .is_empty()
    );
    let wanted = wanted_sessions(
        ReceiverRequestState::default(),
        &plans,
        &monitor,
        Some(&existing),
    );
    assert_eq!(
        wanted.first().map(|plan| plan.target.spec.mode),
        Some(CaptureSessionMode::TouchpadRecovery)
    );
}

#[test]
fn managed_touchpad_session_stays_wanted_without_a_journal_record() {
    let plan = crate::capture_plan::plan_for_device_with_touchpad(
        &openlogi_core::config::Config::default(),
        physical_key(),
        "touchpad-a",
        route(),
        None,
        crate::capture_plan::CapturePlanOptions {
            touchpad_journal_id: Some("unit:01020304".to_string()),
            rearm_generation: 0,
            os_mouse_hook_available: true,
        },
    );
    let plans = Arc::new(vec![plan]);
    let monitor = Arc::new(crate::touchpad_monitor::TouchpadMonitor::default());

    let wanted = wanted_sessions(ReceiverRequestState::default(), &plans, &monitor, None);

    assert_eq!(
        wanted.first().map(|plan| plan.target.spec.mode),
        Some(CaptureSessionMode::Continuous)
    );
}

#[test]
fn diagnostic_can_temporarily_capture_a_disabled_touchpad() {
    let plan = crate::capture_plan::touchpad_recovery_plan(
        physical_key(),
        "touchpad-a",
        route(),
        "unit:01020304".to_string(),
        0,
    );
    let plans = Arc::new(vec![plan]);
    let monitor = Arc::new(crate::touchpad_monitor::TouchpadMonitor::default());
    monitor.poll("touchpad-a");
    let journal = StubJournal(None);

    let wanted = wanted_sessions(
        ReceiverRequestState::default(),
        &plans,
        &monitor,
        Some(&journal),
    );

    assert_eq!(
        wanted.first().map(|plan| plan.target.spec.mode),
        Some(CaptureSessionMode::TouchpadOnly)
    );
    assert!(
        wanted
            .first()
            .is_some_and(|plan| plan.target.spec.capture_touchpad)
    );
}

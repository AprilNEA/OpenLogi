use super::*;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::DeviceRoute;

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

    let plans = Arc::new(std::sync::RwLock::new(vec![plan()]));
    assert!(wanted_sessions(&access, &plans).is_empty());

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

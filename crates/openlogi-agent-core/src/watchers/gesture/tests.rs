use super::*;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};

fn route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc548,
    }
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("mouse-a", epoch)
}

fn stopped_session_with_epoch(epoch: u64) -> RunningSession {
    let plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    RunningSession {
        id: session_id(epoch),
        target: SessionTarget::for_plan(&plan),
        plan,
        stop: None,
    }
}

fn live_session_with_epoch(epoch: u64) -> RunningSession {
    let (stop, _rx) = oneshot::channel();
    RunningSession {
        stop: Some(stop),
        ..stopped_session_with_epoch(epoch)
    }
}

#[test]
fn rearms_when_the_current_session_dies() {
    assert_eq!(
        on_done(&session_id(7), Some(&live_session_with_epoch(7))),
        DoneAction::Remove { unexpected: true }
    );
}

#[test]
fn ignores_a_stale_session_superseded_by_a_restart() {
    assert_eq!(
        on_done(&session_id(6), Some(&live_session_with_epoch(7))),
        DoneAction::Ignore
    );
}

#[test]
fn ignores_a_completion_from_another_device_at_the_same_epoch() {
    assert_eq!(
        on_done(
            &HidppSessionId::with_epoch("mouse-b", 7),
            Some(&live_session_with_epoch(7))
        ),
        DoneAction::Ignore
    );
}

#[test]
fn ignores_a_completion_for_an_untracked_device() {
    assert_eq!(on_done(&session_id(7), None), DoneAction::Ignore);
}

#[test]
fn settles_a_draining_session_quietly() {
    assert_eq!(
        on_done(&session_id(7), Some(&stopped_session_with_epoch(7))),
        DoneAction::Remove { unexpected: false }
    );
}

#[test]
fn accepts_inputs_from_the_current_session_until_teardown_finishes() {
    assert!(dispatch_plan_for(&session_id(7), Some(&live_session_with_epoch(7))).is_some());
    assert!(
        dispatch_plan_for(&session_id(6), Some(&live_session_with_epoch(7))).is_none(),
        "a superseded session's queued input is stale"
    );
    assert!(
        dispatch_plan_for(&session_id(7), Some(&stopped_session_with_epoch(7))).is_some(),
        "a draining session still owns diverted input until restoration completes"
    );
    assert!(dispatch_plan_for(&session_id(7), None).is_none());
}

#[tokio::test]
async fn exclusive_request_stops_rearming_without_rejecting_owned_input() {
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

    let plans = Arc::new(std::sync::RwLock::new(vec![
        live_session_with_epoch(7).plan,
    ]));
    assert!(wanted_sessions(&access, &plans).is_empty());
    assert!(
        dispatch_plan_for(&session_id(7), Some(&live_session_with_epoch(7))).is_some(),
        "the current session owns diverted input until it reports Done"
    );

    pairing.abort();
    let _ = pairing.await;
    drop(session_lease);
}

#[test]
fn side_gesture_transition_keeps_the_retiring_plan_until_native_restore() {
    let mut old_plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        "mouse-a",
        route(),
        None,
        0,
        true,
    );
    old_plan.side_gesture_bindings.insert(
        ButtonId::Forward,
        [(GestureDirection::Click, Action::MissionControl)].into(),
    );
    old_plan
        .divert_gesture_buttons
        .push((0x0056, ButtonId::Forward));
    let mut session = stopped_session_with_epoch(7);
    session.target = SessionTarget::for_plan(&old_plan);
    session.plan = old_plan.clone();

    let mut published_without_hook = old_plan;
    published_without_hook.side_gesture_bindings.clear();
    published_without_hook.divert_gesture_buttons.clear();
    assert!(!session_matches_plan(&session, &published_without_hook));

    let retained = dispatch_plan_for(&session.id, Some(&session))
        .expect("the draining session must remain an admissible input owner");
    assert!(
        retained
            .side_gesture_bindings
            .contains_key(&ButtonId::Forward)
    );
    assert!(
        retained
            .divert_gesture_buttons
            .iter()
            .any(|&(_, button)| button == ButtonId::Forward),
        "the retiring plan must resolve input while firmware diversion remains active"
    );
}

#[test]
fn capture_plan_changes_schedule_the_old_session_for_retirement() {
    let session = live_session_with_epoch(7);
    let mut plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        "mouse-a",
        session.target.route.clone(),
        None,
        0,
        true,
    );
    assert!(session_matches_plan(&session, &plan));

    plan.rearm_generation = 1;
    assert!(
        !session_matches_plan(&session, &plan),
        "a capture-plan epoch change must retire the old session"
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
    let first = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    let mut session = live_session_with_epoch(7);
    session.target = SessionTarget::for_plan(&first);
    session.plan = first.clone();

    config.set_binding(
        "mouse-a",
        ButtonId::DpiToggle,
        Binding::Single(Action::Paste),
    );
    let rebound = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    assert!(session_matches_plan(&session, &rebound));
    assert!(refresh_dispatch_plan(&mut session, Some(&rebound)));
    assert_eq!(
        session.plan.bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn active_session_adopts_gesture_and_per_app_dispatch_changes() {
    let mut config = openlogi_core::config::Config::default();
    config.set_gesture_mode("mouse-a", ButtonId::GestureButton, true);
    let first = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    let mut session = live_session_with_epoch(7);
    session.target = SessionTarget::for_plan(&first);
    session.plan = first;

    config.set_gesture_direction(
        "mouse-a",
        ButtonId::GestureButton,
        GestureDirection::Right,
        Action::MissionControl,
    );
    let gestured = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    assert!(refresh_dispatch_plan(&mut session, Some(&gestured)));
    assert_eq!(
        session
            .plan
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
    let base = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    session.target = SessionTarget::for_plan(&base);
    session.plan = base;
    config.set_per_app_binding(
        "mouse-a",
        "com.example.Editor",
        ButtonId::DpiToggle,
        Some(Action::Paste),
    );
    let per_app = crate::capture_plan::plan_for_device(
        &config,
        "mouse-a",
        route(),
        Some("com.example.Editor"),
        0,
        true,
    );
    assert!(refresh_dispatch_plan(&mut session, Some(&per_app)));
    assert_eq!(
        session.plan.bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn draining_session_keeps_its_retiring_dispatch_plan_frozen() {
    let mut session = stopped_session_with_epoch(7);
    session
        .plan
        .bindings
        .insert(ButtonId::DpiToggle, Binding::Single(Action::Copy));
    let mut published = session.plan.clone();
    published
        .bindings
        .insert(ButtonId::DpiToggle, Binding::Single(Action::Paste));

    assert!(refresh_dispatch_plan(&mut session, Some(&published)));
    assert_eq!(
        session.plan.bindings.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::Copy)),
        "a draining session must keep resolving late input through its retiring bindings"
    );
}

#[test]
fn wheel_configuration_changes_invalidate_the_capture_epoch() {
    let mut config = openlogi_core::config::Config::default();
    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::NextTab),
    );
    let first = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    let mut session = live_session_with_epoch(7);
    session.target = SessionTarget::for_plan(&first);

    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::VolumeUp),
    );
    let rebound = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    assert_eq!(
        spec_for(&first),
        spec_for(&rebound),
        "both custom bindings require the same HID++ diversion"
    );
    assert!(
        !session_matches_plan(&session, &rebound),
        "binding changes must end the epoch even when the divert set is unchanged"
    );

    session.target = SessionTarget::for_plan(&rebound);
    config.set_device_thumbwheel_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MIN));
    let rescaled = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0, true);
    assert_eq!(spec_for(&rebound), spec_for(&rescaled));
    assert!(
        !session_matches_plan(&session, &rescaled),
        "sensitivity changes must not reuse an old action threshold or cooldown"
    );
}

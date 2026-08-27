use super::*;
use openlogi_core::binding::{Action, Binding, ButtonId};

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
    );
    RunningSession {
        id: session_id(epoch),
        target: SessionTarget::for_plan(&plan),
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
fn accepts_inputs_only_from_the_current_live_session() {
    assert!(accepts_input(
        &session_id(7),
        Some(&live_session_with_epoch(7))
    ));
    assert!(
        !accepts_input(&session_id(6), Some(&live_session_with_epoch(7))),
        "a superseded session's queued input is stale"
    );
    assert!(
        !accepts_input(&session_id(7), Some(&stopped_session_with_epoch(7))),
        "a draining session was already canceled"
    );
    assert!(!accepts_input(&session_id(7), None));
}

#[test]
fn rejects_input_after_the_published_capture_plan_changes() {
    let session = live_session_with_epoch(7);
    let mut plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        "mouse-a",
        session.target.route.clone(),
        None,
        0,
    );
    assert!(session_matches_plan(&session, &plan));

    plan.rearm_generation = 1;
    assert!(
        !session_matches_plan(&session, &plan),
        "an input queued before a capture-plan epoch change is stale"
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
    let first = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0);
    let mut session = live_session_with_epoch(7);
    session.target = SessionTarget::for_plan(&first);

    config.set_binding(
        "mouse-a",
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(Action::VolumeUp),
    );
    let rebound = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0);
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
    let rescaled = crate::capture_plan::plan_for_device(&config, "mouse-a", route(), None, 0);
    assert_eq!(spec_for(&rebound), spec_for(&rescaled));
    assert!(
        !session_matches_plan(&session, &rescaled),
        "sensitivity changes must not reuse an old action threshold or cooldown"
    );
}

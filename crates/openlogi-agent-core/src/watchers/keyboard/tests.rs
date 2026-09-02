use super::*;
use openlogi_core::binding::Action;

fn target() -> KeyboardTarget {
    KeyboardTarget {
        route: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc548,
        },
        wanted: BTreeMap::new(),
        wanted_g_keys: BTreeSet::new(),
        wanted_aux_keys: BTreeSet::new(),
    }
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("keyboard-a", epoch)
}

fn dispatch(action: Action) -> KeyboardDispatchPlan {
    KeyboardDispatchPlan {
        config_key: "keyboard-a".to_owned(),
        bindings: BTreeMap::from([(ButtonId::KeySearch, Binding::Single(action))]),
        g_key_profiles: BTreeMap::new(),
        gaming_key_mode: GamingKeyMode::Profiles,
        gaming_button_bindings: BTreeMap::new(),
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
    assert_eq!(session.reconcile(None), ReconcileAction::Retiring);
    session
}

#[tokio::test]
async fn publication_and_receiver_request_change_wanted_state_immediately() {
    let (spec_tx, mut spec_rx) = watch::channel(None);
    let access = ReceiverAccess::default();
    let mut requests = access.subscribe_requests();
    let published = KeyboardSpec {
        config_key: "keyboard-a".to_owned(),
        route: target().route,
        wanted: target().wanted,
        wanted_g_keys: target().wanted_g_keys,
        wanted_aux_keys: target().wanted_aux_keys,
        bindings: dispatch(Action::MissionControl).bindings,
        g_key_profiles: BTreeMap::new(),
        gaming_key_mode: GamingKeyMode::Profiles,
        gaming_button_bindings: BTreeMap::new(),
    };

    spec_tx.send_replace(Some(Arc::new(published)));
    spec_rx
        .changed()
        .await
        .expect("spec publication should remain open");
    assert!(wanted_session(*requests.borrow(), &spec_rx).is_some());

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
        wanted_session(*requests.borrow(), &spec_rx).is_none(),
        "a queued request should retire capture without waiting for a tick"
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
        session.reconcile(Some((&current_target, &new_dispatch))),
        ReconcileAction::DispatchChanged
    );
    assert!(session.is_active());
    assert_eq!(session.dispatch(), &new_dispatch);
}

#[test]
fn target_changes_freeze_dispatch_until_teardown_finishes() {
    let mut session = live_session(7);
    let old_dispatch = session.dispatch().clone();
    let mut replacement = target();
    replacement.wanted.insert(0x00d4, ButtonId::KeySearch);
    let new_dispatch = dispatch(Action::ShowDesktop);

    assert!(
        replacement != *session.target(),
        "the test must require different firmware capture"
    );
    assert_eq!(
        session.reconcile(Some((&replacement, &new_dispatch))),
        ReconcileAction::Retiring
    );
    assert!(!session.is_active());
    assert_eq!(session.dispatch(), &old_dispatch);
}

#[test]
fn suspended_device_io_disables_retry_deadlines() {
    let retry_at = tokio::time::Instant::now() + RETRY_DELAY;

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, None, Some(retry_at)),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(ReceiverRequestState::default(), false, None, Some(retry_at)),
        None,
        "keyboard retries must stay dormant until visible resume",
    );
}

#[test]
fn profiles_mode_uses_m_keys_as_selectors() {
    let mut plan = dispatch(Action::MissionControl);

    assert_eq!(
        profile_selected_by(ButtonId::KeyM2, &plan),
        Some(GKeyProfile::M2)
    );
    plan.gaming_key_mode = GamingKeyMode::NineButtons;
    assert_eq!(profile_selected_by(ButtonId::KeyM2, &plan), None);
}

#[test]
fn g_key_binding_follows_the_active_profile() {
    let mut plan = dispatch(Action::MissionControl);
    plan.g_key_profiles.insert(
        GKeyProfile::M1,
        BTreeMap::from([(ButtonId::KeyG1, Binding::Single(Action::VolumeUp))]),
    );
    plan.g_key_profiles.insert(
        GKeyProfile::M2,
        BTreeMap::from([(ButtonId::KeyG1, Binding::Single(Action::VolumeDown))]),
    );

    assert_eq!(
        binding_for_button(&plan, GKeyProfile::M1, ButtonId::KeyG1).map(Binding::click_action),
        Some(Action::VolumeUp)
    );
    assert_eq!(
        binding_for_button(&plan, GKeyProfile::M2, ButtonId::KeyG1).map(Binding::click_action),
        Some(Action::VolumeDown)
    );
    assert!(binding_for_button(&plan, GKeyProfile::M3, ButtonId::KeyG1).is_none());
}

#[test]
fn nine_button_mode_uses_one_independent_map_for_g_m_and_mr() {
    let mut plan = dispatch(Action::MissionControl);
    plan.gaming_key_mode = GamingKeyMode::NineButtons;
    plan.gaming_button_bindings
        .insert(ButtonId::KeyG1, Binding::Single(Action::VolumeUp));
    plan.gaming_button_bindings
        .insert(ButtonId::KeyM2, Binding::Single(Action::ShowDesktop));
    plan.gaming_button_bindings
        .insert(ButtonId::KeyMr, Binding::Single(Action::Copy));

    for (button, action) in [
        (ButtonId::KeyG1, Action::VolumeUp),
        (ButtonId::KeyM2, Action::ShowDesktop),
        (ButtonId::KeyMr, Action::Copy),
    ] {
        assert_eq!(
            binding_for_button(&plan, GKeyProfile::M3, button).map(Binding::click_action),
            Some(action)
        );
    }
}

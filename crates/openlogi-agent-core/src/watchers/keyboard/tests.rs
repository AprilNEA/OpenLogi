use super::*;
use openlogi_core::binding::Action;

fn target() -> KeyboardTarget {
    KeyboardTarget {
        route: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc548,
        },
        wanted: BTreeMap::new(),
    }
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("keyboard-a", epoch)
}

fn dispatch(action: Action) -> KeyboardDispatchPlan {
    KeyboardDispatchPlan {
        config_key: "keyboard-a".to_owned(),
        bindings: BTreeMap::from([(ButtonId::KeySearch, Binding::Single(action))]),
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
        bindings: dispatch(Action::MissionControl).bindings,
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
    let slot = KeyboardSlot::recovering(None, Some(retry_at));

    assert_eq!(
        next_deadline(ReceiverRequestState::default(), true, Some(&slot)),
        Some(retry_at),
    );
    assert_eq!(
        next_deadline(ReceiverRequestState::default(), false, Some(&slot)),
        None,
        "keyboard retries must stay dormant until visible resume",
    );
}

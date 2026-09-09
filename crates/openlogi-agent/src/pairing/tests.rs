use super::*;

use std::sync::RwLock;

use openlogi_agent_core::DpiCycles;
use openlogi_agent_core::receiver_access::ReceiverAccess;
use openlogi_agent_core::runtime::hook::HookMaps;
use openlogi_agent_core::runtime::scroll::ScrollPreferences;
use openlogi_core::config::VerticalScrollSensitivity;
use openlogi_hid::PairingError;

fn shared_runtime() -> SharedRuntime {
    let (_, capture_plans) = tokio::sync::watch::channel(Arc::new(Vec::new()));
    let (_, keyboard_spec) = tokio::sync::watch::channel(None);
    let (_, host_switch_links) = tokio::sync::watch::channel(Arc::new(Vec::new()));
    SharedRuntime {
        hook_maps: Arc::new(RwLock::new(HookMaps::default())),
        keyboard_bindings: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
        scroll_preferences: Arc::new(ScrollPreferences::new(
            false,
            VerticalScrollSensitivity::DEFAULT,
        )),
        dpi_cycle: Arc::new(RwLock::new(DpiCycles::default())),
        capture_plans,
        capture_channel: Arc::new(RwLock::new(None)),
        channel_registry: openlogi_hid::ChannelRegistry::default(),
        device_io: openlogi_hid::device_io_channel().1,
        channel_pool: openlogi_hid::host::channel_pool(),
        keyboard_spec,
        keyboard_channel: Arc::new(RwLock::new(None)),
        capture_rearm_generation: Arc::new(0.into()),
        receiver_access: ReceiverAccess::default(),
        host_switch_links,
    }
}

fn manager_with_ctrl(ctrl: mpsc::UnboundedSender<Control>) -> PairingManager {
    let (_, upd_rx) = mpsc::unbounded_channel();
    PairingManager {
        ctrl,
        updates: Mutex::new(upd_rx),
        session: Arc::new(StdMutex::new(SessionOwner::default())),
        shared: shared_runtime(),
        observable: Arc::new(ObservableState::new("test".to_string())),
    }
}

async fn start_session(
    manager: &PairingManager,
    ctrl_rx: &mut mpsc::UnboundedReceiver<Control>,
) -> SessionId {
    manager
        .start(ReceiverSelector::First)
        .await
        .expect("test session should start");
    match ctrl_rx.recv().await.expect("start control") {
        Control::Start { session, .. } => session,
        control => panic!("expected start control, got {control:?}"),
    }
}

fn discovered_device() -> DiscoveredDevice {
    DiscoveredDevice {
        address: [1, 2, 3, 4, 5, 6],
        authentication: 0,
        kind: openlogi_hid::pairing::BoltDeviceKind::Unknown,
        name: "existing".to_string(),
    }
}

fn is_idle(manager: &PairingManager) -> bool {
    with_session_owner(&manager.session, |owner| {
        matches!(owner.state, SessionState::Idle)
    })
}

#[tokio::test]
async fn start_rolls_back_pause_when_watcher_send_fails() {
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    drop(ctrl_rx);
    let manager = manager_with_ctrl(ctrl_tx);

    let result = manager.start(ReceiverSelector::First).await;

    assert_eq!(result, Err(PairingCommandError::WatcherUnavailable));
    assert!(is_idle(&manager));
    assert_eq!(manager.observable.snapshot().pairing, None);
    assert!(!manager.shared.receiver_access.exclusive_requested());
    assert!(
        manager
            .shared
            .receiver_access
            .try_acquire_for_session()
            .is_some()
    );
}

#[test]
fn admission_is_owned_and_rolled_back_by_session_identity() {
    let sessions = Arc::new(StdMutex::new(SessionOwner::default()));
    let first =
        SessionAdmission::new(Arc::clone(&sessions)).expect("idle owner should admit a session");

    assert!(matches!(
        SessionAdmission::new(Arc::clone(&sessions)),
        Err(PairingCommandError::AlreadyActive)
    ));
    let first_id = first.id;
    drop(first);

    let second = SessionAdmission::new(Arc::clone(&sessions))
        .expect("dropping an admission should reopen the owner");
    assert_ne!(second.id, first_id);
    with_session_owner(&sessions, |owner| owner.roll_back_admission(first_id));
    assert!(with_session_owner(&sessions, |owner| {
        matches!(owner.state, SessionState::Admitting(id) if id == second.id)
    }));
}

#[tokio::test]
async fn cancel_without_active_session_is_a_noop_success() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);

    let result = manager.cancel();

    assert_eq!(result, Ok(()));
    let sent = ctrl_rx.try_recv();
    assert!(
        sent.is_err(),
        "cancel without an active session must not reach the watcher, got {sent:?}"
    );
}

#[test]
fn pair_without_active_session_does_not_publish_pairing() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    manager
        .observable
        .set_pairing(Some(PairingPhase::Paired { slot: 3 }));

    let result = manager.pair(discovered_device().address);

    assert_eq!(result, Err(PairingCommandError::NoActiveSession));
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Paired { slot: 3 })
    );
    assert!(
        ctrl_rx.try_recv().is_err(),
        "pair without a session must not reach the watcher"
    );
}

#[tokio::test]
async fn audit_queued_device_found_cannot_revert_selected_phase() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    let selected = discovered_device();
    apply_session_event(
        SessionEvent {
            session,
            event: PairingEvent::DeviceFound(selected.clone()),
        },
        &manager.session,
        &manager.observable,
    );
    let queued = DiscoveredDevice {
        address: [6, 5, 4, 3, 2, 1],
        name: "queued second device".to_string(),
        ..discovered_device()
    };
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
    raw_tx
        .send(SessionEvent {
            session,
            event: PairingEvent::DeviceFound(queued.clone()),
        })
        .unwrap();

    manager.pair(selected.address).unwrap();
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Pairing)
    );
    let update = apply_session_event(
        raw_rx.recv().await.unwrap(),
        &manager.session,
        &manager.observable,
    );

    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Pairing)
    );
    assert!(
        update.is_none(),
        "stale discovery must not reach IPC clients"
    );
    assert_eq!(
        manager.pair(queued.address),
        Err(PairingCommandError::UnknownDevice)
    );
    let searching = apply_session_event(
        SessionEvent {
            session,
            event: PairingEvent::Searching,
        },
        &manager.session,
        &manager.observable,
    );
    assert!(searching.is_none());
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Pairing)
    );
}

#[tokio::test]
async fn queued_discovery_cannot_revert_passkey() {
    // Passkeys have always been accepted from the watcher, even without an
    // explicit agent selection. Both paths must close discovery.
    for select_first in [false, true] {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        let session = start_session(&manager, &mut ctrl_rx).await;
        let device = discovered_device();
        apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::DeviceFound(device.clone()),
            },
            &manager.session,
            &manager.observable,
        );
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
        for event in [
            PairingEvent::DeviceFound(DiscoveredDevice {
                address: [6, 5, 4, 3, 2, 1],
                ..discovered_device()
            }),
            PairingEvent::Searching,
        ] {
            raw_tx.send(SessionEvent { session, event }).unwrap();
        }
        if select_first {
            manager.pair(device.address).unwrap();
        }
        let method = openlogi_hid::PasskeyMethod::Keyboard("572901".to_string());
        let update = apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::Passkey(method.clone()),
            },
            &manager.session,
            &manager.observable,
        );
        assert!(matches!(update, Some(PairingUpdate::Passkey(value)) if value == method));
        let expected = Some(PairingPhase::Passkey(method));
        assert_eq!(manager.observable.snapshot().pairing, expected);

        while let Ok(event) = raw_rx.try_recv() {
            assert!(apply_session_event(event, &manager.session, &manager.observable).is_none());
            assert_eq!(manager.observable.snapshot().pairing, expected);
        }
        let terminal = apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::Paired { slot: 2 },
            },
            &manager.session,
            &manager.observable,
        );
        assert!(matches!(terminal, Some(PairingUpdate::Paired { slot: 2 })));
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Paired { slot: 2 })
        );
        assert!(is_idle(&manager));
        assert!(!manager.shared.receiver_access.exclusive_requested());
    }
}

#[tokio::test]
async fn initial_discovery_keeps_devices_available_for_repeat_selection() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    let searching = apply_session_event(
        SessionEvent {
            session,
            event: PairingEvent::Searching,
        },
        &manager.session,
        &manager.observable,
    );
    assert!(matches!(searching, Some(PairingUpdate::Searching)));
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Searching)
    );
    let first = discovered_device();
    let second = DiscoveredDevice {
        address: [6, 5, 4, 3, 2, 1],
        authentication: 1,
        name: "second keyboard".to_string(),
        ..discovered_device()
    };
    let mut found = Vec::new();
    for device in [&first, &second] {
        let expected = FoundDevice {
            address: device.address,
            name: device.name.clone(),
        };
        let update = apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::DeviceFound(device.clone()),
            },
            &manager.session,
            &manager.observable,
        );
        assert!(matches!(update, Some(PairingUpdate::DeviceFound(value)) if value == expected));
        found.push(expected);
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Found(found.clone()))
        );
    }
    for device in [&first, &second, &first] {
        manager.pair(device.address).unwrap();
        let control = ctrl_rx.try_recv().unwrap();
        assert!(
            matches!(control, Control::Pair { session: id, device: sent }
            if id == session && sent.address == device.address
                && sent.authentication == device.authentication && sent.name == device.name)
        );
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Pairing)
        );
        apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::Passkey(openlogi_hid::PasskeyMethod::Keyboard(
                    "572901".to_string(),
                )),
            },
            &manager.session,
            &manager.observable,
        );
    }
    let terminal = apply_session_event(
        SessionEvent {
            session,
            event: PairingEvent::Failed(PairingError::Timeout),
        },
        &manager.session,
        &manager.observable,
    );
    assert!(matches!(
        terminal,
        Some(PairingUpdate::Failed(PairingFailure::Timeout))
    ));
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Failed(PairingFailure::Timeout))
    );
    assert!(is_idle(&manager));
    assert!(!manager.shared.receiver_access.exclusive_requested());
}

#[tokio::test]
async fn unsuccessful_pair_keeps_discovery_open() {
    for error in [
        PairingCommandError::UnknownDevice,
        PairingCommandError::WatcherUnavailable,
    ] {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        let session = start_session(&manager, &mut ctrl_rx).await;
        let first = discovered_device();
        let second = DiscoveredDevice {
            address: [6, 5, 4, 3, 2, 1],
            ..discovered_device()
        };
        apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::DeviceFound(first.clone()),
            },
            &manager.session,
            &manager.observable,
        );
        let before = manager.observable.snapshot().pairing;
        let address = if error == PairingCommandError::WatcherUnavailable {
            ctrl_rx.close();
            first.address
        } else {
            second.address
        };

        assert_eq!(manager.pair(address), Err(error));
        assert_eq!(manager.observable.snapshot().pairing, before);
        assert!(
            ctrl_rx.try_recv().is_err(),
            "failed pair must not send a command"
        );
        let update = apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::DeviceFound(second.clone()),
            },
            &manager.session,
            &manager.observable,
        );
        assert!(matches!(update, Some(PairingUpdate::DeviceFound(found))
            if found.address == second.address));
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Found(vec![
                FoundDevice {
                    address: first.address,
                    name: first.name
                },
                FoundDevice {
                    address: second.address,
                    name: second.name
                },
            ]))
        );
    }
}

#[tokio::test]
async fn start_ignores_overlapping_session_without_clearing_or_sending() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    apply_session_event(
        SessionEvent {
            session,
            event: PairingEvent::DeviceFound(discovered_device()),
        },
        &manager.session,
        &manager.observable,
    );

    let result = manager.start(ReceiverSelector::First).await;

    assert_eq!(result, Err(PairingCommandError::AlreadyActive));
    assert_eq!(
        with_session_owner(&manager.session, |owner| owner
            .active()
            .map(|active| active.devices.len())),
        Some(1)
    );
    let sent = ctrl_rx.try_recv();
    assert!(
        sent.is_err(),
        "an overlapping start must not reach the watcher, got {sent:?}"
    );
}

#[tokio::test]
async fn terminal_cleanup_is_exactly_once_and_releases_receiver_lease() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let first = start_session(&manager, &mut ctrl_rx).await;
    apply_session_event(
        SessionEvent {
            session: first,
            event: PairingEvent::DeviceFound(discovered_device()),
        },
        &manager.session,
        &manager.observable,
    );
    manager.pair(discovered_device().address).unwrap();
    assert!(matches!(ctrl_rx.try_recv().unwrap(), Control::Pair { .. }));
    manager.cancel().unwrap();
    assert!(matches!(ctrl_rx.try_recv().unwrap(), Control::Cancel { session } if session == first));
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Pairing)
    );
    assert!(
        manager
            .shared
            .receiver_access
            .requested(ExclusiveAccessReason::Pairing)
    );

    let first_terminal = apply_session_event(
        SessionEvent {
            session: first,
            event: PairingEvent::Failed(PairingError::Cancelled),
        },
        &manager.session,
        &manager.observable,
    );

    assert!(matches!(
        first_terminal,
        Some(PairingUpdate::Failed(PairingFailure::Cancelled))
    ));
    assert!(is_idle(&manager));
    assert_eq!(manager.observable.snapshot().pairing, None);
    assert!(!manager.shared.receiver_access.exclusive_requested());
    assert!(
        manager
            .shared
            .receiver_access
            .try_acquire_for_session()
            .is_some()
    );

    let second = start_session(&manager, &mut ctrl_rx).await;
    assert_ne!(second, first);
    for event in [
        PairingEvent::Failed(PairingError::Cancelled),
        PairingEvent::DeviceFound(discovered_device()),
        PairingEvent::Searching,
        PairingEvent::Passkey(openlogi_hid::PasskeyMethod::Keyboard("572901".to_string())),
    ] {
        let stale = apply_session_event(
            SessionEvent {
                session: first,
                event,
            },
            &manager.session,
            &manager.observable,
        );
        assert!(stale.is_none());
    }
    assert_eq!(
        manager.pair(discovered_device().address),
        Err(PairingCommandError::UnknownDevice)
    );
    assert_eq!(
        with_session_owner(&manager.session, |owner| owner
            .active()
            .map(|session| session.id)),
        Some(second)
    );
    assert!(manager.shared.receiver_access.exclusive_requested());
    assert_eq!(
        manager.observable.snapshot().pairing,
        Some(PairingPhase::Searching)
    );

    let second_terminal = apply_session_event(
        SessionEvent {
            session: second,
            event: PairingEvent::Paired { slot: 4 },
        },
        &manager.session,
        &manager.observable,
    );
    assert!(matches!(
        second_terminal,
        Some(PairingUpdate::Paired { slot: 4 })
    ));
    assert!(is_idle(&manager));
    assert!(!manager.shared.receiver_access.exclusive_requested());
}

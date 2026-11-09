use super::*;

use openlogi_agent_core::orchestrator::Orchestrator;
use openlogi_core::config::Config;
use openlogi_hid::{DiscoveredDevice, PairingError, PairingEvent};
use openlogi_ipc::{FoundDevice, PairingPhase};

fn shared_handles() -> SharedHandles {
    Orchestrator::new(
        Config::default(),
        Arc::new(ObservableState::new("test".to_string())),
    )
    .shared()
}

fn manager_with_ctrl(ctrl: mpsc::UnboundedSender<PairingControl>) -> PairingManager {
    let (_, upd_rx) = mpsc::unbounded_channel();
    PairingManager {
        ctrl,
        updates: Mutex::new(upd_rx),
        session: Arc::new(published_session()),
        shared: shared_handles(),
    }
}

fn published_session() -> PublishedSession {
    PublishedSession::new(Arc::new(ObservableState::new("test".to_string())))
}

fn published(manager: &PairingManager) -> Option<PairingPhase> {
    manager.session.observable.snapshot().pairing
}

async fn start_session(
    manager: &PairingManager,
    ctrl_rx: &mut mpsc::UnboundedReceiver<PairingControl>,
) -> PairingSessionId {
    manager
        .start(ReceiverSelector::First)
        .await
        .expect("test session should start");
    match ctrl_rx.recv().await.expect("start control") {
        PairingControl::Start { session, .. } => session,
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

#[tokio::test]
async fn cancellation_acknowledges_only_after_terminal_cleanup() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    let mut cancel = std::pin::pin!(manager.cancel());
    assert!(
        futures::poll!(&mut cancel).is_pending(),
        "cancel acknowledged before receiver cleanup"
    );
    assert!(matches!(
        ctrl_rx.try_recv().unwrap(),
        PairingControl::Cancel { .. }
    ));
    apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Failed(PairingError::Cancelled),
        },
        &manager.session,
    );
    cancel.await.unwrap();
    assert!(!manager.shared.receiver_access.exclusive_requested());
    manager
        .start(ReceiverSelector::First)
        .await
        .expect("the next choice can start immediately after acknowledgement");
}

#[tokio::test]
async fn cancellation_dismisses_a_result_that_won_the_terminal_race() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    let mut cancel = std::pin::pin!(manager.cancel());
    assert!(futures::poll!(&mut cancel).is_pending());
    assert!(matches!(
        ctrl_rx.try_recv().unwrap(),
        PairingControl::Cancel { .. }
    ));
    apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Paired { slot: 1 },
        },
        &manager.session,
    );
    cancel.await.unwrap();
    assert_eq!(
        published(&manager),
        None,
        "cancellation must also dismiss a simultaneous terminal result"
    );
}

#[tokio::test]
async fn late_cancellation_completion_does_not_dismiss_a_replacement_result() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let first = start_session(&manager, &mut ctrl_rx).await;
    let mut cancel = std::pin::pin!(manager.cancel());
    assert!(futures::poll!(&mut cancel).is_pending());
    assert!(matches!(
        ctrl_rx.try_recv().unwrap(),
        PairingControl::Cancel { .. }
    ));
    apply_session_event(
        PairingSessionEvent {
            session: first,
            event: PairingEvent::Failed(PairingError::Cancelled),
        },
        &manager.session,
    );
    let second = start_session(&manager, &mut ctrl_rx).await;
    apply_session_event(
        PairingSessionEvent {
            session: second,
            event: PairingEvent::Paired { slot: 2 },
        },
        &manager.session,
    );
    cancel.await.unwrap();
    assert_eq!(published(&manager), Some(PairingPhase::Paired { slot: 2 }));
}

#[tokio::test]
async fn start_rolls_back_pause_when_watcher_send_fails() {
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    drop(ctrl_rx);
    let manager = manager_with_ctrl(ctrl_tx);

    let result = manager.start(ReceiverSelector::First).await;

    assert_eq!(result, Err(PairingCommandError::WatcherUnavailable));
    assert_eq!(published(&manager), None);
    assert!(!manager.shared.receiver_access.exclusive_requested());
    assert!(
        manager
            .shared
            .receiver_access
            .try_acquire_for_session()
            .is_some()
    );
    assert_eq!(
        manager.start(ReceiverSelector::First).await,
        Err(PairingCommandError::WatcherUnavailable),
        "a failed start must not leave the owner admitting"
    );
}

#[test]
fn admission_is_owned_and_rolled_back_by_session_identity() {
    let sessions = Arc::new(published_session());
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
    sessions.transition(|owner| owner.roll_back_admission(first_id));
    assert!(
        matches!(
            SessionAdmission::new(Arc::clone(&sessions)),
            Err(PairingCommandError::AlreadyActive)
        ),
        "a stale rollback must leave the newer admission in place"
    );
}

#[tokio::test]
async fn cancel_without_active_session_is_a_noop_success() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);

    let result = manager.cancel().await;

    assert_eq!(result, Ok(()));
    let sent = ctrl_rx.try_recv();
    assert!(
        sent.is_err(),
        "cancel without an active session must not reach the watcher, got {sent:?}"
    );
}

#[tokio::test]
async fn pair_without_active_session_does_not_publish_pairing() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Paired { slot: 3 },
        },
        &manager.session,
    );

    let result = manager.pair(discovered_device().address);

    assert_eq!(result, Err(PairingCommandError::NoActiveSession));
    assert_eq!(published(&manager), Some(PairingPhase::Paired { slot: 3 }));
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
        PairingSessionEvent {
            session,
            event: PairingEvent::DeviceFound(selected.clone()),
        },
        &manager.session,
    );
    let queued = DiscoveredDevice {
        address: [6, 5, 4, 3, 2, 1],
        name: "queued second device".to_string(),
        ..discovered_device()
    };
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
    raw_tx
        .send(PairingSessionEvent {
            session,
            event: PairingEvent::DeviceFound(queued.clone()),
        })
        .unwrap();

    manager.pair(selected.address).unwrap();
    assert_eq!(published(&manager), Some(PairingPhase::Pairing));
    let update = apply_session_event(raw_rx.recv().await.unwrap(), &manager.session);

    assert_eq!(published(&manager), Some(PairingPhase::Pairing));
    assert!(
        update.is_none(),
        "stale discovery must not reach IPC clients"
    );
    assert_eq!(
        manager.pair(queued.address),
        Err(PairingCommandError::UnknownDevice)
    );
    let searching = apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Searching,
        },
        &manager.session,
    );
    assert!(searching.is_none());
    assert_eq!(published(&manager), Some(PairingPhase::Pairing));
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
            PairingSessionEvent {
                session,
                event: PairingEvent::DeviceFound(device.clone()),
            },
            &manager.session,
        );
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
        for event in [
            PairingEvent::DeviceFound(DiscoveredDevice {
                address: [6, 5, 4, 3, 2, 1],
                ..discovered_device()
            }),
            PairingEvent::Searching,
        ] {
            raw_tx.send(PairingSessionEvent { session, event }).unwrap();
        }
        if select_first {
            manager.pair(device.address).unwrap();
        }
        let method = openlogi_hid::PasskeyMethod::Keyboard("572901".to_string());
        let update = apply_session_event(
            PairingSessionEvent {
                session,
                event: PairingEvent::Passkey(method.clone()),
            },
            &manager.session,
        );
        assert!(matches!(update, Some(PairingUpdate::Passkey(value)) if value == method));
        let expected = Some(PairingPhase::Passkey(method));
        assert_eq!(published(&manager), expected);

        while let Ok(event) = raw_rx.try_recv() {
            assert!(apply_session_event(event, &manager.session).is_none());
            assert_eq!(published(&manager), expected);
        }
        let terminal = apply_session_event(
            PairingSessionEvent {
                session,
                event: PairingEvent::Paired { slot: 2 },
            },
            &manager.session,
        );
        assert!(matches!(terminal, Some(PairingUpdate::Paired { slot: 2 })));
        assert_eq!(published(&manager), Some(PairingPhase::Paired { slot: 2 }));
        assert!(!manager.shared.receiver_access.exclusive_requested());
    }
}

#[tokio::test]
async fn initial_discovery_keeps_devices_available_for_repeat_selection() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    let session = start_session(&manager, &mut ctrl_rx).await;
    let searching = apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Searching,
        },
        &manager.session,
    );
    assert!(matches!(searching, Some(PairingUpdate::Searching)));
    assert_eq!(published(&manager), Some(PairingPhase::Searching));
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
            PairingSessionEvent {
                session,
                event: PairingEvent::DeviceFound(device.clone()),
            },
            &manager.session,
        );
        assert!(matches!(update, Some(PairingUpdate::DeviceFound(value)) if value == expected));
        found.push(expected);
        assert_eq!(
            published(&manager),
            Some(PairingPhase::Found(found.clone()))
        );
    }
    for device in [&first, &second, &first] {
        manager.pair(device.address).unwrap();
        let control = ctrl_rx.try_recv().unwrap();
        assert!(
            matches!(control, PairingControl::Pair { session: id, device: sent }
            if id == session && sent.address == device.address
                && sent.authentication == device.authentication && sent.name == device.name)
        );
        assert_eq!(published(&manager), Some(PairingPhase::Pairing));
        apply_session_event(
            PairingSessionEvent {
                session,
                event: PairingEvent::Passkey(openlogi_hid::PasskeyMethod::Keyboard(
                    "572901".to_string(),
                )),
            },
            &manager.session,
        );
    }
    let terminal = apply_session_event(
        PairingSessionEvent {
            session,
            event: PairingEvent::Failed(PairingError::Timeout),
        },
        &manager.session,
    );
    assert!(matches!(
        terminal,
        Some(PairingUpdate::Failed(PairingFailure::Timeout))
    ));
    assert_eq!(
        published(&manager),
        Some(PairingPhase::Failed(PairingFailure::Timeout))
    );
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
            PairingSessionEvent {
                session,
                event: PairingEvent::DeviceFound(first.clone()),
            },
            &manager.session,
        );
        let before = published(&manager);
        let address = if error == PairingCommandError::WatcherUnavailable {
            ctrl_rx.close();
            first.address
        } else {
            second.address
        };

        assert_eq!(manager.pair(address), Err(error));
        assert_eq!(published(&manager), before);
        assert!(
            ctrl_rx.try_recv().is_err(),
            "failed pair must not send a command"
        );
        let update = apply_session_event(
            PairingSessionEvent {
                session,
                event: PairingEvent::DeviceFound(second.clone()),
            },
            &manager.session,
        );
        assert!(matches!(update, Some(PairingUpdate::DeviceFound(found))
            if found.address == second.address));
        assert_eq!(
            published(&manager),
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
        PairingSessionEvent {
            session,
            event: PairingEvent::DeviceFound(discovered_device()),
        },
        &manager.session,
    );

    let result = manager.start(ReceiverSelector::First).await;

    assert_eq!(result, Err(PairingCommandError::AlreadyActive));
    let device = discovered_device();
    assert_eq!(
        published(&manager),
        Some(PairingPhase::Found(vec![FoundDevice {
            address: device.address,
            name: device.name,
        }]))
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
        PairingSessionEvent {
            session: first,
            event: PairingEvent::DeviceFound(discovered_device()),
        },
        &manager.session,
    );
    manager.pair(discovered_device().address).unwrap();
    assert!(matches!(
        ctrl_rx.try_recv().unwrap(),
        PairingControl::Pair { .. }
    ));
    let mut cancel = std::pin::pin!(manager.cancel());
    assert!(futures::poll!(&mut cancel).is_pending());
    assert!(
        matches!(ctrl_rx.try_recv().unwrap(), PairingControl::Cancel { session } if session == first)
    );
    assert_eq!(published(&manager), Some(PairingPhase::Pairing));
    assert!(
        manager
            .shared
            .receiver_access
            .requested(ExclusiveAccessReason::Pairing)
    );

    assert!(matches!(
        apply_session_event(
            PairingSessionEvent {
                session: first,
                event: PairingEvent::Failed(PairingError::Cancelled),
            },
            &manager.session,
        ),
        Some(PairingUpdate::Failed(PairingFailure::Cancelled))
    ));
    cancel.await.unwrap();
    assert_eq!(published(&manager), None);
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
            PairingSessionEvent {
                session: first,
                event,
            },
            &manager.session,
        );
        assert!(stale.is_none());
    }
    assert_eq!(
        manager.pair(discovered_device().address),
        Err(PairingCommandError::UnknownDevice)
    );
    assert_eq!(
        manager
            .session
            .transition(|owner| owner.subscribe_completion().map(|(id, _)| id)),
        Some(second)
    );
    assert!(manager.shared.receiver_access.exclusive_requested());
    assert_eq!(published(&manager), Some(PairingPhase::Searching));

    let second_terminal = apply_session_event(
        PairingSessionEvent {
            session: second,
            event: PairingEvent::Paired { slot: 4 },
        },
        &manager.session,
    );
    assert!(matches!(
        second_terminal,
        Some(PairingUpdate::Paired { slot: 4 })
    ));
    assert!(!manager.shared.receiver_access.exclusive_requested());
}

#[tokio::test(start_paused = true)]
async fn unpair_is_refused_while_a_pairing_session_holds_the_receiver() {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let manager = manager_with_ctrl(ctrl_tx);
    start_session(&manager, &mut ctrl_rx).await;

    let route = DeviceRoute::Bolt {
        receiver_uid: "F00DCAFE".into(),
        slot: 2,
    };
    assert_eq!(
        manager.unpair(&route).await,
        Err(PairingFailure::ReceiverBusy)
    );
}

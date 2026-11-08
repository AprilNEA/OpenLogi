use super::*;

use openlogi_agent_core::receiver_access::{ExclusiveAccessReason, ReceiverAccess};
use openlogi_hid::PairingError;
use openlogi_hid::pairing::BoltDeviceKind;

fn lease() -> ExclusiveReceiverLease {
    futures::executor::block_on(
        ReceiverAccess::default().acquire_exclusive(ExclusiveAccessReason::Pairing),
    )
}

/// Admit and activate the next session, as `PairingManager::start` does.
fn live(owner: &mut SessionOwner) -> PairingSessionId {
    let id = owner.begin_admission().expect("the owner is idle");
    owner
        .activate(id, lease(), |_| Ok(()))
        .expect("the start command is delivered");
    id
}

fn on(session: PairingSessionId, event: PairingEvent) -> PairingSessionEvent {
    PairingSessionEvent { session, event }
}

fn device(id: u8) -> DiscoveredDevice {
    DiscoveredDevice {
        address: [id, 0, 0, 0, 0, 0],
        authentication: 0,
        kind: BoltDeviceKind::Unknown,
        name: format!("device {id}"),
    }
}

fn found(id: u8) -> FoundDevice {
    found_device(&device(id))
}

#[test]
fn a_result_reported_right_after_activation_is_not_overwritten() {
    let mut owner = SessionOwner::default();
    let id = live(&mut owner);
    assert_eq!(owner.phase(), Some(PairingPhase::Searching));

    // The watcher can fail the session as soon as it has the start command.
    owner.apply(on(id, PairingEvent::Failed(PairingError::ReceiverNotFound)));
    let failed = Some(PairingPhase::Failed(PairingFailure::ReceiverNotFound));
    assert_eq!(owner.phase(), failed);

    // Nothing left of that admission can bring `Searching` back.
    owner.roll_back_admission(id);
    assert_eq!(
        owner.activate(id, lease(), |_| Ok(())),
        Err(PairingCommandError::WatcherUnavailable)
    );
    assert_eq!(owner.phase(), failed);
}

#[test]
fn selection_closes_discovery_only_once_its_command_is_sent() {
    let mut owner = SessionOwner::default();
    let id = live(&mut owner);
    owner.apply(on(id, PairingEvent::DeviceFound(device(1))));

    let refused = owner.select(device(1).address, |_, _| {
        Err(PairingCommandError::WatcherUnavailable)
    });
    assert_eq!(refused, Err(PairingCommandError::WatcherUnavailable));
    assert!(
        owner
            .apply(on(id, PairingEvent::DeviceFound(device(2))))
            .is_some(),
        "a refused selection leaves discovery open"
    );
    assert_eq!(
        owner.phase(),
        Some(PairingPhase::Found(vec![found(1), found(2)]))
    );

    let mut sent = None;
    owner
        .select(device(2).address, |session, device| {
            sent = Some((session, device.address));
            Ok(())
        })
        .expect("the selection is delivered");
    assert_eq!(sent, Some((id, device(2).address)));
    assert_eq!(owner.phase(), Some(PairingPhase::Pairing));

    // Discovery the watcher queued before the selection is dropped.
    for queued in [
        PairingEvent::DeviceFound(device(3)),
        PairingEvent::Searching,
    ] {
        assert!(owner.apply(on(id, queued)).is_none());
    }
    assert_eq!(owner.phase(), Some(PairingPhase::Pairing));
    assert_eq!(
        owner.select(device(3).address, |_, _| Ok(())),
        Err(PairingCommandError::UnknownDevice)
    );
}

#[test]
fn a_terminal_event_ends_its_session_exactly_once() {
    let mut owner = SessionOwner::default();
    let first = live(&mut owner);
    let (_, completion) = owner.subscribe_completion().expect("a live session");

    assert!(matches!(
        owner.apply(on(first, PairingEvent::Paired { slot: 1 })),
        Some(PairingUpdate::Paired { slot: 1 })
    ));
    assert!(
        completion.has_changed().is_err(),
        "ending the session closes its completion"
    );
    assert!(
        owner
            .apply(on(first, PairingEvent::Failed(PairingError::Timeout)))
            .is_none()
    );
    assert_eq!(owner.phase(), Some(PairingPhase::Paired { slot: 1 }));

    let second = live(&mut owner);
    assert!(
        owner
            .apply(on(first, PairingEvent::Failed(PairingError::Cancelled)))
            .is_none(),
        "a stale terminal must not end the replacement session"
    );
    assert_eq!(owner.phase(), Some(PairingPhase::Searching));
    assert_eq!(owner.subscribe_completion().map(|(id, _)| id), Some(second));
}

#[test]
fn cancellation_dismisses_only_the_cancelled_sessions_result() {
    let mut owner = SessionOwner::default();
    let first = live(&mut owner);
    // The device paired before the watcher saw the cancel command.
    owner.apply(on(first, PairingEvent::Paired { slot: 1 }));
    owner.dismiss_result_of(first);
    assert_eq!(owner.phase(), None);

    let second = live(&mut owner);
    owner.apply(on(second, PairingEvent::Paired { slot: 2 }));
    owner.dismiss_result_of(first);
    assert_eq!(owner.phase(), Some(PairingPhase::Paired { slot: 2 }));

    live(&mut owner);
    owner.dismiss_result_of(second);
    assert_eq!(owner.phase(), Some(PairingPhase::Searching));
}

#[test]
fn an_admission_shows_the_previous_result_until_its_session_is_live() {
    let mut owner = SessionOwner::default();
    let first = live(&mut owner);
    owner.apply(on(first, PairingEvent::Failed(PairingError::Timeout)));
    let timed_out = Some(PairingPhase::Failed(PairingFailure::Timeout));

    let second = owner.begin_admission().expect("the owner is idle");
    assert_eq!(owner.phase(), timed_out);
    owner.roll_back_admission(second);
    assert_eq!(owner.phase(), timed_out);

    let third = owner
        .begin_admission()
        .expect("the rollback reopened the owner");
    assert_ne!(third, second);
    owner
        .activate(third, lease(), |_| Ok(()))
        .expect("the start command is delivered");
    assert_eq!(owner.phase(), Some(PairingPhase::Searching));
}

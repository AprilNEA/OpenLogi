use std::collections::BTreeMap;

use openlogi_core::device::DeviceKind;
use openlogi_flow::identity::CanonicalDeviceIdentifier;

use super::*;
use crate::flow::{FlowDeviceSnapshot, RuntimeDevice};

fn device(key: &str, unit: u32, online: bool) -> RuntimeDevice {
    let identity = proto::DeviceIdentity {
        ids: vec![CanonicalDeviceIdentifier::unit_id(unit).into()],
        ..Default::default()
    };
    RuntimeDevice {
        snapshot: FlowDeviceSnapshot {
            config_key: key.into(),
            route: None,
            serial: None,
            unit_id: unit.to_be_bytes(),
            kind: DeviceKind::Mouse,
            online,
        },
        identity,
        channels: BTreeMap::new(),
    }
}

fn request(id: u64, devices: &[RuntimeDevice]) -> proto::HandoffRequest {
    let mut request = proto::HandoffRequest {
        transfer_id: id,
        devices: devices
            .iter()
            .map(|device| device.identity.clone())
            .collect(),
        ..Default::default()
    };
    *request.entry.get_or_insert_default() = proto::EntryPoint {
        side: proto::Side::Left.into(),
        t: 0.5,
        ..Default::default()
    };
    request
}

#[tokio::test]
async fn receiver_arms_before_accept_and_dedupes_same_transfer() {
    let book = HandoffBook::default();
    let peer = PublicKey::new([7; 32]);
    let devices = vec![device("mouse", 1, false)];
    let request = request(42, &devices);

    assert!(matches!(
        book.arm(peer, &request, &devices, true).await,
        ArmDecision::New(_)
    ));
    assert!(matches!(
        book.arm(peer, &request, &devices, true).await,
        ArmDecision::Duplicate(_)
    ));
    assert!(book.mark_accepted(peer, 42).await);
}

#[tokio::test]
async fn receiver_rejects_conflict_unknown_and_already_online() {
    let book = HandoffBook::default();
    let peer = PublicKey::new([7; 32]);
    let offline = vec![device("mouse", 1, false)];
    assert!(matches!(
        book.arm(peer, &request(1, &offline), &offline, true).await,
        ArmDecision::New(_)
    ));
    assert!(matches!(
        book.arm(peer, &request(2, &offline), &offline, true).await,
        ArmDecision::Reject(proto::HandoffReject { reason, .. })
            if reason.as_known() == Some(proto::HandoffRejectReason::AlreadyPending)
    ));

    let other = HandoffBook::default();
    let online = vec![device("mouse", 1, true)];
    assert!(matches!(
        other.arm(peer, &request(3, &online), &online, true).await,
        ArmDecision::Reject(proto::HandoffReject { reason, .. })
            if reason.as_known() == Some(proto::HandoffRejectReason::NotReady)
    ));
    assert!(matches!(
        other
            .arm(peer, &request(4, &[device("unknown", 2, false)]), &online, true)
            .await,
        ArmDecision::Reject(proto::HandoffReject { reason, .. })
            if reason.as_known() == Some(proto::HandoffRejectReason::UnknownDevice)
    ));
}

#[tokio::test]
async fn receiver_reports_arrived_partial_and_timeout_from_fresh_inventory() {
    let peer = PublicKey::new([7; 32]);
    let configured = vec![device("mouse", 1, false), device("keyboard", 2, false)];

    let book = HandoffBook::default();
    assert!(matches!(
        book.arm(peer, &request(5, &configured), &configured, true)
            .await,
        ArmDecision::New(_)
    ));
    assert!(book.mark_accepted(peer, 5).await);
    let arrived = vec![device("mouse", 1, true), device("keyboard", 2, true)];
    let completed = book.observe(&arrived).await.unwrap();
    assert_eq!(
        completed.result.outcome.as_known(),
        Some(proto::HandoffOutcome::Arrived)
    );

    let book = HandoffBook::default();
    assert!(matches!(
        book.arm(peer, &request(6, &configured), &configured, true)
            .await,
        ArmDecision::New(_)
    ));
    assert!(book.mark_accepted(peer, 6).await);
    let partial = vec![device("mouse", 1, true), device("keyboard", 2, false)];
    assert!(book.observe(&partial).await.is_none());
    let completed = book.expire(peer, 6).await.unwrap();
    assert_eq!(
        completed.result.outcome.as_known(),
        Some(proto::HandoffOutcome::Partial)
    );

    let book = HandoffBook::default();
    assert!(matches!(
        book.arm(peer, &request(7, &configured), &configured, true)
            .await,
        ArmDecision::New(_)
    ));
    assert!(book.mark_accepted(peer, 7).await);
    let completed = book.expire(peer, 7).await.unwrap();
    assert_eq!(
        completed.result.outcome.as_known(),
        Some(proto::HandoffOutcome::Timeout)
    );
}

#[tokio::test]
async fn stale_result_does_not_consume_another_peer_waiter() {
    let book = HandoffBook::default();
    let expected = PublicKey::new([1; 32]);
    let stale = PublicKey::new([2; 32]);
    let mut receiver = book.register_outgoing(expected, 9).await;
    let result = proto::HandoffResult {
        transfer_id: 9,
        outcome: proto::HandoffOutcome::Arrived.into(),
        ..Default::default()
    };
    assert!(!book.deliver_result(stale, result.clone()).await);
    assert!(receiver.try_recv().is_err());
    assert!(book.deliver_result(expected, result).await);
    assert!(matches!(receiver.await, Ok(OutgoingSignal::Result(_))));
}

#[tokio::test]
async fn completed_transfer_is_replayed_without_rearming() {
    let book = HandoffBook::default();
    let peer = PublicKey::new([7; 32]);
    let offline = vec![device("mouse", 1, false)];
    let request = request(10, &offline);
    assert!(matches!(
        book.arm(peer, &request, &offline, true).await,
        ArmDecision::New(_)
    ));
    assert!(book.mark_accepted(peer, 10).await);
    let completed = book
        .observe(&[device("mouse", 1, true)])
        .await
        .expect("online inventory completes the accepted transfer");

    assert!(matches!(
        book.arm(peer, &request, &offline, true).await,
        ArmDecision::Replay { accept, result }
            if accept.transfer_id == 10 && result == completed.result
    ));
}

#[tokio::test]
async fn response_failure_cannot_disarm_accepted_transfer_but_cancel_can() {
    let book = HandoffBook::default();
    let peer = PublicKey::new([7; 32]);
    let offline = vec![device("mouse", 1, false)];
    let request = request(11, &offline);
    assert!(matches!(
        book.arm(peer, &request, &offline, true).await,
        ArmDecision::New(_)
    ));
    assert!(book.mark_accepted(peer, 11).await);

    assert!(!book.cancel_unaccepted(peer, 11).await);
    assert!(matches!(
        book.arm(peer, &request, &offline, true).await,
        ArmDecision::Duplicate(_)
    ));
    assert!(book.cancel_incoming(peer, 11).await);
    assert!(matches!(
        book.arm(peer, &request, &offline, true).await,
        ArmDecision::New(_)
    ));
}

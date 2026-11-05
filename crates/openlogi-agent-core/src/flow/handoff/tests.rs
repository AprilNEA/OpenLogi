use std::time::Duration;

use openlogi_core::device::DeviceKind;
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use tokio::time::Instant;

use super::{
    ARM_TIMEOUT, AfterSwitch, ArmDecision, CancelOutcome, CompletedTransfer, IncomingLedger,
    SwitchOutcome, SwitchSchedule, after_switch, entry_point,
};
use crate::flow::devices::LocalDevices;
use crate::flow::edge::{EdgeCrossing, EdgeSide, Velocity};
use crate::flow::test_support::{KEYBOARD, MOUSE, config, configured, record, unit_identity};

const SENDER: PublicKey = PublicKey::new([1; 32]);
const OTHER: PublicKey = PublicKey::new([2; 32]);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// The mouse and keyboard as this host sees them.
fn devices(mouse_online: bool, keyboard_online: bool) -> LocalDevices {
    let config = config(vec![
        configured(MOUSE, Some(1)),
        configured(KEYBOARD, Some(1)),
    ]);
    LocalDevices::join(
        &config,
        &[
            record(MOUSE, DeviceKind::Mouse, 0x0a, mouse_online),
            record(KEYBOARD, DeviceKind::Keyboard, 0x0b, keyboard_online),
        ],
    )
}

fn request(transfer_id: u64, units: &[u8]) -> proto::HandoffRequest {
    let mut request = proto::HandoffRequest {
        transfer_id,
        devices: units.iter().copied().map(unit_identity).collect(),
        ..Default::default()
    };
    let entry = request.entry.get_or_insert_default();
    entry.side = proto::Side::Left.into();
    entry.t = 0.25;
    request
}

fn arm(ledger: &mut IncomingLedger, transfer_id: u64, now: Instant) {
    let decision = ledger.arm(
        SENDER,
        &request(transfer_id, &[0x0a, 0x0b]),
        &devices(false, false),
        true,
        now,
    );
    assert!(matches!(decision, ArmDecision::Armed(_)), "{decision:?}");
}

fn reject_reason(decision: &ArmDecision) -> proto::HandoffRejectReason {
    match decision {
        ArmDecision::Rejected(reject) => reject.reason.as_known().expect("known reason"),
        other => panic!("expected a reject, got {other:?}"),
    }
}

fn outcome(done: &CompletedTransfer) -> proto::HandoffOutcome {
    done.result.outcome.as_known().expect("known outcome")
}

#[test]
fn transfer_places_the_cursor_on_the_first_pointing_arrival_and_completes_on_the_last() {
    let mut ledger = IncomingLedger::default();
    let t0 = Instant::now();
    let decision = ledger.arm(
        SENDER,
        &request(7, &[0x0a, 0x0b]),
        &devices(false, false),
        true,
        t0,
    );
    let ArmDecision::Armed(accept) = decision else {
        panic!("expected an accept, got {decision:?}");
    };
    assert_eq!(accept.transfer_id, 7);
    assert_eq!(u128::from(accept.arm_timeout_ms), ARM_TIMEOUT.as_millis());
    assert_eq!(ledger.deadline(), Some(t0 + ARM_TIMEOUT));

    let first = ledger.observe(&devices(true, false), t0 + ms(100));
    let entry = first.entry.expect("the mouse arrival places the cursor");
    assert_eq!(entry.side.as_known(), Some(proto::Side::Left));
    assert!(first.completed.is_none());

    let again = ledger.observe(&devices(true, false), t0 + ms(200));
    assert!(again.entry.is_none(), "the cursor is placed once");

    let done = ledger
        .observe(&devices(true, true), t0 + ms(300))
        .completed
        .expect("every device arrived");
    assert_eq!(done.peer, SENDER);
    assert_eq!(outcome(&done), proto::HandoffOutcome::Arrived);
    let elapsed: Vec<_> = done
        .result
        .arrivals
        .iter()
        .map(|arrival| arrival.elapsed_ms)
        .collect();
    assert_eq!(elapsed, [100, 300]);
    assert!(ledger.deadline().is_none());
}

#[test]
fn expiry_reports_partial_after_an_arrival_and_timeout_without_one() {
    let mut ledger = IncomingLedger::default();
    let t0 = Instant::now();
    arm(&mut ledger, 1, t0);
    ledger.observe(&devices(true, false), t0 + ms(50));
    assert!(ledger.expire(t0 + ARM_TIMEOUT - ms(1)).is_none());
    let done = ledger.expire(t0 + ARM_TIMEOUT).expect("the window closed");
    assert_eq!(outcome(&done), proto::HandoffOutcome::Partial);
    assert_eq!(
        done.result
            .arrivals
            .iter()
            .map(|arrival| arrival.arrived)
            .collect::<Vec<_>>(),
        [true, false]
    );

    arm(&mut ledger, 2, t0);
    let done = ledger.expire(t0 + ARM_TIMEOUT).expect("the window closed");
    assert_eq!(outcome(&done), proto::HandoffOutcome::Timeout);
}

#[test]
fn retransmissions_are_answered_from_the_transfer_state() {
    let mut ledger = IncomingLedger::default();
    let t0 = Instant::now();
    arm(&mut ledger, 7, t0);
    let offline = devices(false, false);

    let duplicate = ledger.arm(SENDER, &request(7, &[0x0a]), &offline, true, t0 + ms(1000));
    let ArmDecision::Duplicate(accept) = duplicate else {
        panic!("expected a duplicate, got {duplicate:?}");
    };
    assert_eq!(accept.arm_timeout_ms, 2000, "the window left");

    let other = ledger.arm(SENDER, &request(8, &[0x0a]), &offline, true, t0);
    assert_eq!(
        reject_reason(&other),
        proto::HandoffRejectReason::AlreadyPending
    );

    let done = ledger.expire(t0 + ARM_TIMEOUT).expect("the window closed");
    let replay = ledger.arm(SENDER, &request(7, &[0x0a]), &offline, true, t0 + ms(4000));
    let ArmDecision::Replay { accept, result } = replay else {
        panic!("expected a replay, got {replay:?}");
    };
    assert_eq!(accept.arm_timeout_ms, 0, "a late sender must not write");
    assert_eq!(result, done.result);

    // The same transfer id from another peer is a different transfer.
    let fresh = ledger.arm(OTHER, &request(7, &[0x0a]), &offline, true, t0);
    assert!(matches!(fresh, ArmDecision::Armed(_)), "{fresh:?}");
}

#[test]
fn cancel_disarms_only_before_any_arrival() {
    let mut ledger = IncomingLedger::default();
    let t0 = Instant::now();
    arm(&mut ledger, 1, t0);
    assert_eq!(ledger.cancel(OTHER, 1), CancelOutcome::Unknown);
    assert_eq!(ledger.cancel(SENDER, 2), CancelOutcome::Unknown);
    assert_eq!(ledger.cancel(SENDER, 1), CancelOutcome::Disarmed);
    assert!(ledger.deadline().is_none());
    let replay = ledger.arm(
        SENDER,
        &request(1, &[0x0a]),
        &devices(false, false),
        true,
        t0,
    );
    let ArmDecision::Replay { result, .. } = replay else {
        panic!("expected a replay, got {replay:?}");
    };
    assert_eq!(
        result.outcome.as_known(),
        Some(proto::HandoffOutcome::Cancelled)
    );

    arm(&mut ledger, 2, t0);
    ledger.observe(&devices(true, false), t0 + ms(10));
    assert_eq!(ledger.cancel(SENDER, 2), CancelOutcome::Ignored);
    let done = ledger
        .observe(&devices(true, true), t0 + ms(20))
        .completed
        .expect("the transfer still completes");
    assert_eq!(outcome(&done), proto::HandoffOutcome::Arrived);
}

#[test]
fn unacceptable_requests_are_rejected_without_arming() {
    let mut ledger = IncomingLedger::default();
    let now = Instant::now();
    let offline = devices(false, false);
    let mut cases = vec![(
        ledger.arm(SENDER, &request(1, &[0x0a]), &offline, false, now),
        proto::HandoffRejectReason::Disabled,
    )];

    let mut bad_entries = Vec::new();
    for t in [1.5, f64::NAN] {
        let mut bad = request(1, &[0x0a]);
        bad.entry.get_or_insert_default().t = t;
        bad_entries.push(bad);
    }
    let mut no_side = request(1, &[0x0a]);
    no_side.entry.get_or_insert_default().side = proto::Side::Unspecified.into();
    bad_entries.push(no_side);
    bad_entries.push(proto::HandoffRequest {
        transfer_id: 1,
        devices: vec![unit_identity(0x0a)],
        ..Default::default()
    });
    bad_entries.push(request(0, &[0x0a]));
    bad_entries.push(request(1, &[]));
    for bad in &bad_entries {
        cases.push((
            ledger.arm(SENDER, bad, &offline, true, now),
            proto::HandoffRejectReason::NotReady,
        ));
    }

    cases.push((
        ledger.arm(SENDER, &request(1, &[0x0c]), &offline, true, now),
        proto::HandoffRejectReason::UnknownDevice,
    ));
    cases.push((
        ledger.arm(SENDER, &request(1, &[0x0a, 0x0a]), &offline, true, now),
        proto::HandoffRejectReason::UnknownDevice,
    ));
    cases.push((
        ledger.arm(
            SENDER,
            &request(1, &[0x0a]),
            &devices(true, false),
            true,
            now,
        ),
        proto::HandoffRejectReason::NotReady,
    ));

    for (decision, reason) in &cases {
        assert_eq!(reject_reason(decision), *reason, "{decision:?}");
    }
    assert!(ledger.deadline().is_none(), "no rejected request arms");
}

#[test]
fn the_oldest_finished_transfer_is_forgotten_past_the_limit() {
    let mut ledger = IncomingLedger::default();
    let now = Instant::now();
    for transfer_id in 1..=65 {
        arm(&mut ledger, transfer_id, now);
        assert_eq!(ledger.cancel(SENDER, transfer_id), CancelOutcome::Disarmed);
    }
    let offline = devices(false, false);
    let newest = ledger.arm(SENDER, &request(65, &[0x0a]), &offline, true, now);
    assert!(matches!(newest, ArmDecision::Replay { .. }), "{newest:?}");
    let oldest = ledger.arm(SENDER, &request(1, &[0x0a]), &offline, true, now);
    assert!(matches!(oldest, ArmDecision::Armed(_)), "{oldest:?}");
}

#[test]
fn sender_writes_only_in_the_first_half_of_a_capped_window() {
    let t0 = Instant::now();
    let accept = |arm_timeout_ms| proto::HandoffAccept {
        transfer_id: 1,
        arm_timeout_ms,
        ..Default::default()
    };

    let schedule = SwitchSchedule::new(t0, t0, &accept(3000));
    assert!(schedule.may_write(t0 + ms(1499)));
    assert!(!schedule.may_write(t0 + ms(1500)));
    assert_eq!(schedule.give_up(), t0 + ms(5000));

    let capped = SwitchSchedule::new(t0, t0, &accept(u32::MAX));
    assert!(capped.may_write(t0 + ms(4999)));
    assert!(!capped.may_write(t0 + ms(5000)));
    assert_eq!(capped.give_up(), t0 + ms(12_000));

    assert!(!SwitchSchedule::new(t0, t0, &accept(0)).may_write(t0));

    // The peer may have opened its window as soon as the request arrived,
    // so an accept that took 1.2 s leaves 0.3 s to start a write, and none
    // once the accept took longer than half the window.
    let slow = SwitchSchedule::new(t0, t0 + ms(1200), &accept(3000));
    assert!(slow.may_write(t0 + ms(1200)));
    assert!(!slow.may_write(t0 + ms(1500)));
    assert_eq!(slow.give_up(), t0 + ms(6200));
    let late = SwitchSchedule::new(t0, t0 + ms(1600), &accept(3000));
    assert!(!late.may_write(t0 + ms(1600)));
}

#[test]
fn sender_cancels_only_when_no_write_started() {
    use SwitchOutcome::{Failed, NotWritten, Written};
    assert_eq!(
        after_switch(&[NotWritten, NotWritten], false),
        AfterSwitch::Cancel(proto::HandoffCancelReason::SwitchFailed)
    );
    assert_eq!(
        after_switch(&[NotWritten], true),
        AfterSwitch::Cancel(proto::HandoffCancelReason::Shutdown)
    );
    assert_eq!(
        after_switch(&[NotWritten, Failed], false),
        AfterSwitch::AwaitResult
    );
    assert_eq!(
        after_switch(&[Written, NotWritten], true),
        AfterSwitch::AwaitResult
    );
}

#[test]
fn entry_point_enters_the_receiver_through_the_opposite_side() {
    let cases = [
        (EdgeSide::Left, proto::Side::Right),
        (EdgeSide::Right, proto::Side::Left),
        (EdgeSide::Top, proto::Side::Bottom),
        (EdgeSide::Bottom, proto::Side::Top),
    ];
    for (exit, enter) in cases {
        let entry = entry_point(&EdgeCrossing {
            side: exit,
            t: 0.4,
            velocity: Velocity { x: 120.0, y: -3.0 },
        });
        assert_eq!(entry.side.as_known(), Some(enter));
        assert!((entry.t - 0.4).abs() < f64::EPSILON);
        let velocity = entry.velocity.as_option().expect("velocity is sent");
        assert!((velocity.x - 120.0).abs() < f64::EPSILON);
        assert!((velocity.y + 3.0).abs() < f64::EPSILON);
    }
}

/// After an agent restart a Unifying device away on the sender has no cached
/// probe, and a Bluetooth device connected to the sender is not enumerated
/// here at all: the inventory lists neither until it connects. A request
/// for such a configured device still arms, and its arrival completes it.
#[test]
fn a_configured_device_the_inventory_does_not_list_yet_can_arrive() {
    let config = config(vec![configured(MOUSE, Some(1))]);
    let mut ledger = IncomingLedger::default();
    let t0 = Instant::now();

    let decision = ledger.arm(
        SENDER,
        &request(5, &[0x0a]),
        &LocalDevices::join(&config, &[]),
        true,
        t0,
    );
    assert!(matches!(decision, ArmDecision::Armed(_)), "{decision:?}");

    let arrived = LocalDevices::join(&config, &[record(MOUSE, DeviceKind::Mouse, 0x0a, true)]);
    let observed = ledger.observe(&arrived, t0 + ms(80));
    assert!(
        observed.entry.is_some(),
        "the arriving mouse places the cursor"
    );
    let done = observed.completed.expect("the only device arrived");
    assert_eq!(outcome(&done), proto::HandoffOutcome::Arrived);
    assert_eq!(
        done.result.arrivals[0]
            .device
            .as_option()
            .map(|device| device.ids.clone()),
        Some(unit_identity(0x0a).ids)
    );

    let unconfigured = ledger.arm(
        SENDER,
        &request(6, &[0x0b]),
        &LocalDevices::join(&config, &[]),
        true,
        t0,
    );
    assert_eq!(
        reject_reason(&unconfigured),
        proto::HandoffRejectReason::UnknownDevice
    );
}

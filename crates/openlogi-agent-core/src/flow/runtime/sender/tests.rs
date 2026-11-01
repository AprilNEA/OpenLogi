use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use openlogi_core::device::DeviceKind;
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::transport::{
    FlowConnection, FlowEndpoint, MachineIdentity, NotificationEvent, PeerTrust, RpcEvent,
    message_envelope,
};
use openlogi_hid::{DeviceIoSignal, HostSwitchError, device_io_channel};
use tokio::sync::{oneshot, watch};

use super::{
    Negotiation, SendReport, Switch, acquire_and_prepare, negotiate, new_transfer_id, prepare_all,
};
use crate::flow::devices::LocalDevices;
use crate::flow::runtime::hello;
use crate::flow::test_support::{
    KEYBOARD, MOUSE, config, configured, no_hardware, peer, record, unit_identity,
};

const WAIT: Duration = Duration::from_secs(5);

/// A scripted write: counts that it started, then takes `delay`.
struct FakeSwitch {
    started: Arc<AtomicUsize>,
    delay: Duration,
}

impl Switch for FakeSwitch {
    async fn apply(self) -> Result<bool, HostSwitchError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        Ok(true)
    }
}

/// Two endpoints over loopback, connected.
struct Pair {
    _endpoints: (FlowEndpoint, FlowEndpoint),
    sender: Arc<FlowConnection>,
    peer: FlowConnection,
}

async fn pair() -> Pair {
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let local = MachineIdentity::generate().expect("sender identity");
    let local_key = local.public_key();
    let remote = MachineIdentity::generate().expect("peer identity");
    let remote_key = remote.public_key();
    let sender = FlowEndpoint::bind(
        loopback,
        local,
        PeerTrust::pinned([remote_key]),
        hello(local_key),
    )
    .expect("bind the sender");
    let peer = FlowEndpoint::bind(
        loopback,
        remote,
        PeerTrust::pinned([local_key]),
        hello(remote_key),
    )
    .expect("bind the peer");
    let address = peer.local_addr().expect("peer address");
    let (outgoing, incoming) = tokio::time::timeout(WAIT, async {
        tokio::join!(sender.connect(address), peer.accept())
    })
    .await
    .expect("connect in time");
    Pair {
        sender: Arc::new(outgoing.expect("connect")),
        peer: incoming.expect("accept"),
        _endpoints: (sender, peer),
    }
}

struct Run {
    started: Arc<AtomicUsize>,
    /// Controls the device-I/O gate the writes check; it starts open.
    io: DeviceIoSignal,
    result: oneshot::Sender<proto::HandoffResult>,
    stop: watch::Sender<bool>,
    report: tokio::task::JoinHandle<SendReport>,
}

/// Start negotiating transfer `transfer_id` for two devices whose writes take
/// `delay`.
fn start(pair: &Pair, transfer_id: u64, delay: Duration, stopped: bool) -> Run {
    let started = Arc::new(AtomicUsize::new(0));
    let (result, result_rx) = oneshot::channel();
    let (stop, stop_rx) = watch::channel(stopped);
    let (io, device_io) = device_io_channel();
    let switch = || FakeSwitch {
        started: Arc::clone(&started),
        delay,
    };
    let mut entry = proto::EntryPoint {
        side: proto::Side::Left.into(),
        t: 0.25,
        ..Default::default()
    };
    entry.velocity.get_or_insert_default().x = -1.0;
    let negotiation = Negotiation {
        connection: Arc::clone(&pair.sender),
        transfer_id,
        entry,
        switches: vec![
            (unit_identity(0x0a), switch()),
            (unit_identity(0x0b), switch()),
        ],
        result: result_rx,
        stop: stop_rx,
        device_io,
    };
    Run {
        started,
        io,
        result,
        stop,
        report: tokio::spawn(negotiate(negotiation, ())),
    }
}

/// Answer the peer's next request with `answer`; return the request.
async fn answer(
    peer: &FlowConnection,
    answer: impl FnOnce(&proto::HandoffRequest) -> Envelope,
) -> proto::HandoffRequest {
    answer_after(peer, Duration::ZERO, answer).await
}

/// Answer the peer's next request with `answer`, `delay` after it arrives.
async fn answer_after(
    peer: &FlowConnection,
    delay: Duration,
    answer: impl FnOnce(&proto::HandoffRequest) -> Envelope,
) -> proto::HandoffRequest {
    let event = tokio::time::timeout(WAIT, peer.accept_rpc())
        .await
        .expect("a request arrives in time")
        .expect("the request is valid");
    let RpcEvent::Request(rpc) = event else {
        panic!("the transport rejected the request");
    };
    let request: proto::HandoffRequest = rpc
        .request()
        .decode(InboundRole::Request)
        .expect("decode the request");
    tokio::time::sleep(delay).await;
    rpc.respond(answer(&request)).await.expect("respond");
    request
}

fn accept(transfer_id: u64, arm_timeout_ms: u32) -> Envelope {
    let accept = proto::HandoffAccept {
        transfer_id,
        arm_timeout_ms,
        ..Default::default()
    };
    message_envelope(FrameKind::HandoffAccept, &accept).expect("encode the accept")
}

fn arrived(transfer_id: u64) -> proto::HandoffResult {
    proto::HandoffResult {
        transfer_id,
        outcome: proto::HandoffOutcome::Arrived.into(),
        ..Default::default()
    }
}

/// The transfer's report, and how many writes started.
async fn report(
    report: tokio::task::JoinHandle<SendReport>,
    started: &AtomicUsize,
) -> (SendReport, usize) {
    let report = tokio::time::timeout(WAIT, report)
        .await
        .expect("the transfer ends in time")
        .expect("the transfer task does not panic");
    (report, started.load(Ordering::SeqCst))
}

async fn next_cancel(peer: &FlowConnection) -> proto::HandoffCancel {
    let event = tokio::time::timeout(WAIT, peer.accept_notification())
        .await
        .expect("a cancel arrives in time")
        .expect("the connection stays open");
    let NotificationEvent::Notification(envelope) = event else {
        panic!("expected a notification, got {event:?}");
    };
    assert_eq!(envelope.kind, FrameKind::HandoffCancel);
    envelope
        .decode(InboundRole::Notification)
        .expect("decode the cancel")
}

#[tokio::test]
async fn an_accepted_transfer_writes_every_device_and_reports_the_result() {
    let pair = pair().await;
    let run = start(&pair, 7, Duration::ZERO, false);
    let request = answer(&pair.peer, |request| accept(request.transfer_id, 5_000)).await;
    assert_eq!(request.transfer_id, 7);
    assert_eq!(
        request.devices,
        vec![unit_identity(0x0a), unit_identity(0x0b)]
    );
    let entry = request.entry.as_option().expect("the request has an entry");
    assert_eq!(entry.side, proto::Side::Left);
    assert!(request.sent_at_ms > 0);
    run.result.send(arrived(7)).expect("the sender waits");

    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(report, SendReport::Finished(arrived(7)));
    assert_eq!(started, 2);
}

#[tokio::test]
async fn a_rejected_transfer_writes_nothing() {
    let pair = pair().await;
    let run = start(&pair, 8, Duration::ZERO, false);
    answer(&pair.peer, |request| {
        let reject = proto::HandoffReject {
            transfer_id: request.transfer_id,
            reason: proto::HandoffRejectReason::AlreadyPending.into(),
            ..Default::default()
        };
        message_envelope(FrameKind::HandoffReject, &reject).expect("encode the reject")
    })
    .await;

    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(
        report,
        SendReport::Rejected(proto::HandoffRejectReason::AlreadyPending)
    );
    assert_eq!(started, 0);
}

#[tokio::test]
async fn no_write_starts_after_the_write_window() {
    let pair = pair().await;
    // The first write takes 300 ms; writes may start for 200 ms after the
    // request.
    let run = start(&pair, 9, Duration::from_millis(300), false);
    answer(&pair.peer, |request| accept(request.transfer_id, 400)).await;
    run.result.send(arrived(9)).expect("the sender waits");

    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(report, SendReport::Finished(arrived(9)));
    assert_eq!(started, 1);
}

#[tokio::test]
async fn a_slow_accept_leaves_no_time_to_write() {
    let pair = pair().await;
    // The peer's 400 ms window may open when the request arrives, so its
    // accept 300 ms later is past the 200 ms in which a write may start.
    let run = start(&pair, 13, Duration::ZERO, false);
    answer_after(&pair.peer, Duration::from_millis(300), |request| {
        accept(request.transfer_id, 400)
    })
    .await;

    let cancel = next_cancel(&pair.peer).await;
    assert_eq!(cancel.transfer_id, 13);
    let (report, started) = report(run.report, &run.started).await;
    assert!(matches!(report, SendReport::Cancelled(_)), "{report:?}");
    assert_eq!(started, 0);
}

#[tokio::test]
async fn an_accept_for_another_transfer_cancels_without_writing() {
    let pair = pair().await;
    let run = start(&pair, 10, Duration::ZERO, false);
    answer(&pair.peer, |_| accept(99, 5_000)).await;

    let cancel = next_cancel(&pair.peer).await;
    assert_eq!(cancel.transfer_id, 10);
    assert_eq!(cancel.reason, proto::HandoffCancelReason::SwitchFailed);
    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(
        report,
        SendReport::Cancelled(proto::HandoffCancelReason::SwitchFailed)
    );
    assert_eq!(started, 0);
}

#[tokio::test]
async fn stopping_before_the_writes_cancels_with_shutdown() {
    let pair = pair().await;
    let run = start(&pair, 11, Duration::ZERO, true);
    answer(&pair.peer, |request| accept(request.transfer_id, 5_000)).await;

    let cancel = next_cancel(&pair.peer).await;
    assert_eq!(cancel.reason, proto::HandoffCancelReason::Shutdown);
    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(
        report,
        SendReport::Cancelled(proto::HandoffCancelReason::Shutdown)
    );
    assert_eq!(started, 0);
}

/// Sleep or a user switch can suspend device I/O while the request waits for
/// its answer. No write may start then, and the peer is told to disarm.
#[tokio::test]
async fn device_io_suspended_before_the_writes_cancels_without_writing() {
    let pair = pair().await;
    let run = start(&pair, 14, Duration::ZERO, false);
    assert!(run.io.suspend(), "the gate starts open");
    answer(&pair.peer, |request| accept(request.transfer_id, 5_000)).await;

    let cancel = next_cancel(&pair.peer).await;
    assert_eq!(cancel.transfer_id, 14);
    assert_eq!(cancel.reason, proto::HandoffCancelReason::SwitchFailed);
    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(
        report,
        SendReport::Cancelled(proto::HandoffCancelReason::SwitchFailed)
    );
    assert_eq!(started, 0);
}

#[tokio::test]
async fn stopping_while_waiting_for_the_result_ends_the_transfer() {
    let pair = pair().await;
    let run = start(&pair, 12, Duration::ZERO, false);
    answer(&pair.peer, |request| accept(request.transfer_id, 5_000)).await;
    // Let the writes finish, then stop instead of answering.
    tokio::time::sleep(Duration::from_millis(50)).await;
    run.stop.send_replace(true);

    let (report, started) = report(run.report, &run.started).await;
    assert_eq!(report, SendReport::NoResult);
    assert_eq!(started, 2);
}

#[tokio::test]
async fn the_hardware_phase_needs_open_device_io_and_a_prepared_device() {
    let config = config(vec![configured(MOUSE, Some(1))]);
    let devices = LocalDevices::join(&config, &[record(MOUSE, DeviceKind::Mouse, 0x0a, true)])
        .outgoing(&peer());
    assert_eq!(devices.len(), 1);
    let (signal, hardware) = no_hardware();

    let unprepared = acquire_and_prepare(&hardware, &devices).await.err();
    assert_eq!(
        unprepared,
        Some(SendReport::NotStarted("a device could not be prepared"))
    );

    assert!(signal.suspend());
    let suspended = acquire_and_prepare(&hardware, &devices).await.err();
    assert_eq!(
        suspended,
        Some(SendReport::NotStarted("device I/O is suspended"))
    );
}

#[tokio::test]
async fn one_device_that_cannot_switch_keeps_every_device_here() {
    let config = config(vec![
        configured(MOUSE, Some(1)),
        configured(KEYBOARD, Some(1)),
    ]);
    let records = [
        record(MOUSE, DeviceKind::Mouse, 0x0a, true),
        record(KEYBOARD, DeviceKind::Keyboard, 0x0b, true),
    ];
    let devices = LocalDevices::join(&config, &records).outgoing(&peer());
    assert_eq!(devices.len(), 2);
    let started = Arc::new(AtomicUsize::new(0));
    let switch = || FakeSwitch {
        started: Arc::clone(&started),
        delay: Duration::ZERO,
    };

    // The mouse prepares; the keyboard's slot for the peer is empty.
    let split = prepare_all(&devices, |device| {
        let prepared = if device.identity == devices[0].identity {
            Ok(Some(switch()))
        } else {
            Err(HostSwitchError::HostSlotEmpty { host: 1 })
        };
        std::future::ready(prepared)
    })
    .await
    .err();
    assert_eq!(
        split,
        Some(SendReport::NotStarted("a device could not be prepared"))
    );

    // A device already on the peer's slot needs no write.
    let moved = prepare_all(&devices, |device| {
        let prepared = (device.identity == devices[0].identity).then(switch);
        std::future::ready(Ok(prepared))
    })
    .await
    .expect("the mouse still needs its switch");
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].0, devices[0].identity);
}

#[test]
fn transfer_ids_are_distinct_and_never_zero() {
    let ids: std::collections::HashSet<u64> = (0..1_000).map(|_| new_transfer_id()).collect();
    assert_eq!(ids.len(), 1_000);
    assert!(!ids.contains(&0));
}

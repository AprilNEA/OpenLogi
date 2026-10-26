use super::*;
use crate::hardware::{DeviceAccess, TransportTurns, WriteOrder};
use crate::receiver_access::ReceiverAccess;
use openlogi_device::test_support::publish_scripted_channel;
use openlogi_hid::{ChannelRegistry, DeviceRoute, device_io_channel};
use std::sync::atomic::AtomicU8;
use std::time::Duration;

struct Fixture {
    op: DeviceOp,
    order: WriteOrder<Option<DisableKeysMask>>,
    disabled: Arc<AtomicU8>,
    abandoned: Arc<AtomicBool>,
    reports: openlogi_device::replay::ReplayChannelHandle,
}

impl Fixture {
    async fn new() -> Self {
        let registry = ChannelRegistry::default();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let disabled = Arc::new(AtomicU8::new(0x80));
        let current = Arc::clone(&disabled);
        let (_, reports) = publish_scripted_channel(
            &registry,
            "buffered-completion",
            route.clone(),
            move |request| {
                let value = match (request[2], request[3] >> 4) {
                    (0, 1) => 4,
                    (0, 0) if request[4..6] == [0x45, 0x21] => 5,
                    (5, 0) => 0xa1,
                    (5, 1) => current.load(Ordering::Acquire),
                    (5, 2) => {
                        current.store(request[4], Ordering::Release);
                        0
                    }
                    _ => return None,
                };
                Some(vec![0x10, request[1], request[2], request[3], value, 0, 0])
            },
        )
        .await;
        let (_, device_io) = device_io_channel();
        let access = DeviceAccess {
            channel: Arc::default(),
            registry,
            receiver_access: ReceiverAccess::default(),
            device_io,
            transport_turns: TransportTurns::default(),
        };
        let order = WriteOrder::default();
        order.sync_policy(&route, Some(DisableKeysMask::EMPTY));
        Self {
            op: access.op(&route),
            order,
            disabled,
            abandoned: Arc::new(AtomicBool::new(false)),
            reports,
        }
    }

    fn start(&self, desired: DisableKeysMask) -> DisableKeysJob {
        let abandoned = Arc::clone(&self.abandoned);
        let pending = self
            .order
            .begin(&self.op.route, Some(desired))
            .on_abandoned(move || {
                abandoned.store(true, Ordering::Release);
            });
        write_manual_disable_keys(self.op.access.op(&self.op.route), pending, desired).unwrap()
    }
}

async fn cancel_before_io(job: DisableKeysJob) {
    let DisableKeysJob {
        cancellation,
        started,
        result,
        worker,
    } = job;
    drop(cancellation);
    // Do not block the runtime's shutdown on a join if the worker regresses.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !worker.is_finished() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("cancelled worker exits while its predecessor remains blocked");
    worker.join().expect("cancelled worker joins");
    assert!(started.await.is_err(), "cancelled request never starts I/O");
    assert!(matches!(
        result
            .await
            .expect("completion")
            .accept()
            .expect_err("cancelled"),
        WriteError::RequestTimedOut {
            operation: HidppOperation::WriteDisableKeys,
        }
    ));
}

#[tokio::test]
async fn cancellation_wakes_before_and_after_wait_registration() {
    for already_cancelled in [false, true] {
        let cancellation = Cancellation::default();
        if already_cancelled {
            cancellation.request();
        }
        let mut waiting = Box::pin(cancellation.wait());
        if !already_cancelled {
            assert!(
                futures_lite::future::poll_once(&mut waiting)
                    .await
                    .is_none()
            );
            cancellation.request();
        }
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("cancellation cannot lose a wake-up");
    }
}

#[tokio::test]
async fn cancelled_setting_wait_finishes_without_acquiring_the_turn() {
    let fixture = Fixture::new().await;
    let (_, ticket) = fixture.order.policy(&fixture.op.route).unwrap();
    let turn = ticket.turn().await.unwrap();
    cancel_before_io(fixture.start(DisableKeysMask::CAPS_LOCK)).await;
    assert!(fixture.abandoned.load(Ordering::Acquire));
    assert!(fixture.reports.written_reports().is_empty());
    drop(turn);
    let state = fixture.start(DisableKeysMask::EMPTY).wait().await.unwrap();
    assert_eq!(state.disabled.bits(), 0x80);
}

#[tokio::test]
async fn cancelled_receiver_wait_releases_the_setting_turn() {
    use crate::receiver_access::ExclusiveAccessReason;

    let fixture = Fixture::new().await;
    let lease = fixture
        .op
        .access
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    cancel_before_io(fixture.start(DisableKeysMask::CAPS_LOCK)).await;
    assert!(fixture.abandoned.load(Ordering::Acquire));
    assert!(fixture.reports.written_reports().is_empty());
    let (_, ticket) = fixture.order.policy(&fixture.op.route).unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(2), ticket.turn())
        .await
        .expect("setting turn released")
        .expect("policy restored");
    drop((turn, lease));
}

#[tokio::test]
async fn cancelled_transport_wait_releases_the_receiver_lease() {
    use crate::receiver_access::ExclusiveAccessReason;

    let fixture = Fixture::new().await;
    let transport = fixture
        .op
        .access
        .transport_turns
        .turn_for(&fixture.op.route);
    let turn = transport.lock().await;
    cancel_before_io(fixture.start(DisableKeysMask::CAPS_LOCK)).await;
    assert!(fixture.abandoned.load(Ordering::Acquire));
    assert!(fixture.reports.written_reports().is_empty());
    let exclusive = tokio::time::timeout(
        Duration::from_secs(2),
        fixture
            .op
            .access
            .receiver_access
            .acquire_exclusive(ExclusiveAccessReason::Pairing),
    )
    .await
    .expect("receiver lease released while transport remains held");
    drop((exclusive, turn));
}

#[tokio::test]
async fn cancelled_reservation_finishes_behind_a_held_native_write() {
    use openlogi_fixture::RequestMatch;

    let fixture = Fixture::new().await;
    let held = fixture
        .reports
        .hold_next_write_completion(RequestMatch::Hidpp20, &[0x10, 0xff, 5, 0x20, 0x81, 0, 0]);
    let first = fixture.start(DisableKeysMask::CAPS_LOCK);
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .expect("predecessor native write is held");
    let reports = fixture.reports.written_reports();
    cancel_before_io(fixture.start(DisableKeysMask::EMPTY)).await;
    assert!(
        !first.worker.is_finished(),
        "native predecessor is still held"
    );
    assert!(fixture.abandoned.load(Ordering::Acquire));
    assert_eq!(
        fixture.reports.written_reports(),
        reports,
        "cancelled request sent no I/O"
    );

    let mut third = fixture.start(DisableKeysMask::EMPTY);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut third.started)
            .await
            .is_err(),
        "third request must not skip the native predecessor"
    );
    assert_eq!(fixture.reports.written_reports(), reports);
    held.release();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), first.wait())
            .await
            .expect("predecessor drains")
            .expect_err("third owns intent"),
        WriteError::WriteSuperseded { .. }
    ));
    let state = tokio::time::timeout(Duration::from_secs(2), third.wait())
        .await
        .expect("third request is not stranded")
        .expect("third completes");
    assert_eq!(state.disabled.bits(), 0x80);
    assert_eq!(fixture.disabled.load(Ordering::Acquire), 0x80);
}

#[tokio::test]
async fn buffered_success_abandoned_before_receipt_restores_accepted_policy() {
    let fixture = Fixture::new().await;
    let DisableKeysJob {
        cancellation,
        started,
        result,
        worker,
    } = fixture.start(DisableKeysMask::CAPS_LOCK);
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || worker.join().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        fixture.disabled.load(Ordering::Acquire),
        0x81,
        "native write completed"
    );
    drop((cancellation, started, result));
    assert!(
        fixture.abandoned.load(Ordering::Acquire),
        "an unconsumed success must release pending intent as abandoned"
    );
    assert!(fixture.order.schedule_recovery(&fixture.op.route));
    assert_eq!(
        fixture.order.policy(&fixture.op.route).unwrap().0,
        Some(DisableKeysMask::EMPTY)
    );
}

#[tokio::test]
async fn buffered_error_keeps_pending_ownership_until_receipt_or_cancellation() {
    let fixture = Fixture::new().await;
    let DisableKeysJob {
        cancellation,
        started,
        result,
        worker,
    } = fixture.start(DisableKeysMask::from_bits_retain(0x40));
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || worker.join().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !fixture.abandoned.load(Ordering::Acquire),
        "buffering a result is not requester completion"
    );
    assert!(!fixture.order.schedule_recovery(&fixture.op.route));
    drop((cancellation, started, result));
    assert!(fixture.abandoned.load(Ordering::Acquire));
    assert!(fixture.order.schedule_recovery(&fixture.op.route));
    assert_eq!(fixture.disabled.load(Ordering::Acquire), 0x80);
}

#[tokio::test]
async fn received_success_is_the_accepted_recovery_policy() {
    let fixture = Fixture::new().await;
    let result = fixture
        .start(DisableKeysMask::CAPS_LOCK)
        .wait()
        .await
        .unwrap();
    assert_eq!(result.disabled.bits(), 0x81);
    assert!(!fixture.abandoned.load(Ordering::Acquire));
    assert!(!fixture.order.schedule_recovery(&fixture.op.route));
    drop(
        fixture
            .order
            .begin(&fixture.op.route, Some(DisableKeysMask::EMPTY)),
    );
    assert!(fixture.order.schedule_recovery(&fixture.op.route));
    assert_eq!(
        fixture.order.policy(&fixture.op.route).unwrap().0,
        Some(DisableKeysMask::CAPS_LOCK)
    );
}

#[tokio::test]
async fn buffered_success_cannot_confirm_after_a_successor_takes_priority() {
    let fixture = Fixture::new().await;
    let DisableKeysJob {
        cancellation,
        started,
        result,
        worker,
    } = fixture.start(DisableKeysMask::CAPS_LOCK);
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || worker.join().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    let successor = fixture
        .order
        .begin(&fixture.op.route, Some(DisableKeysMask::EMPTY));
    assert!(matches!(
        result.await.unwrap().accept().unwrap_err(),
        WriteError::WriteSuperseded { .. }
    ));
    drop((cancellation, started, successor));
    assert!(fixture.order.schedule_recovery(&fixture.op.route));
    assert_eq!(
        fixture.order.policy(&fixture.op.route).unwrap().0,
        Some(DisableKeysMask::EMPTY),
        "an unaccepted buffered result cannot become the recovery baseline"
    );
}

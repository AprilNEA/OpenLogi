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
        publish_scripted_channel(
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

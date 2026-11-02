//! Disable Keys native-write ownership regression.

use super::*;
use crate::orchestrator::SharedHandles;
use openlogi_core::hid::DisableKeysMask;
use openlogi_device::test_support::publish_scripted_channel;
use openlogi_fixture::RequestMatch;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Keyboard {
    route: DeviceRoute,
    writes: Arc<Mutex<Vec<u8>>>,
    reports: openlogi_device::replay::ReplayChannelHandle,
}

// Readback repeats the same query header: consume a released first barrier
// before holding the second report. These are raw-completion, not reply gates.
const TRANSACTION_REPORTS: [([u8; 7], bool); 7] = [
    ([0x10, 0xff, 0, 0x10, 0, 0, 0], false),
    ([0x10, 0xff, 0, 0, 0x45, 0x21, 0], false),
    ([0x10, 0xff, 5, 0, 0, 0, 0], false),
    ([0x10, 0xff, 5, 0x10, 0, 0, 0], false),
    ([0x10, 0xff, 5, 0x20, 0x81, 0, 0], false),
    ([0x10, 0xff, 5, 0, 0, 0, 0], true),
    ([0x10, 0xff, 5, 0x10, 0, 0, 0], true),
];

impl Keyboard {
    async fn publish(shared: &SharedHandles) -> Self {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let disabled = Arc::new(AtomicU8::new(0x80));
        let device_disabled = Arc::clone(&disabled);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let device_writes = Arc::clone(&writes);
        let (_, reports) = publish_scripted_channel(
            &shared.channel_registry,
            "native-write-keyboard",
            route.clone(),
            move |request| {
                if request.len() < 7 {
                    return None;
                }
                let value = match (request[2], request[3] >> 4) {
                    (0, 1) => 4,
                    (0, 0) => {
                        if request[4..6] == [0x45, 0x21] {
                            5
                        } else {
                            0
                        }
                    }
                    (5, 0) => 0xa1,
                    (5, 1) => device_disabled.load(Ordering::SeqCst),
                    (5, 2) => {
                        device_writes.lock().expect("writes").push(request[4]);
                        device_disabled.store(request[4], Ordering::SeqCst);
                        request[4]
                    }
                    _ => return None,
                };
                Some(vec![0x10, request[1], request[2], request[3], value, 0, 0])
            },
        )
        .await;
        Self {
            route,
            writes,
            reports,
        }
    }

    fn hold_set_write(&self, value: u8) -> openlogi_device::replay::ReplayWriteBarrier {
        self.reports
            .hold_next_write_completion(RequestMatch::Hidpp20, &[0x10, 0xff, 5, 0x20, value, 0, 0])
    }

    fn hold_stage(&self, stage: usize) -> openlogi_device::replay::ReplayWriteBarrier {
        let (report, readback) = TRANSACTION_REPORTS[stage];
        if readback {
            self.reports
                .hold_next_write_completion(RequestMatch::Hidpp20, &report)
                .release();
        }
        self.reports
            .hold_next_write_completion(RequestMatch::Hidpp20, &report)
    }
}

#[tokio::test]
async fn background_policy_retains_ownership_through_every_native_report() {
    use crate::receiver_access::ExclusiveAccessReason;

    let mut cases = tokio::task::JoinSet::new();
    for stage in 0..TRANSACTION_REPORTS.len() {
        cases.spawn(async move {
            let shared = orchestrator(Config::default()).shared();
            let keyboard = Keyboard::publish(&shared).await;
            shared
                .disabled_keys_order
                .sync_policy(&keyboard.route, Some(DisableKeysMask::CAPS_LOCK));
            let held = keyboard.hold_stage(stage);
            let worker = shared.reapply_disabled_keys(&keyboard.route).unwrap();
            tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
                .await
                .unwrap();
            let mut finished = tokio::task::spawn_blocking(move || worker.join().unwrap());
            // Exceed the real inner 5-second HID++ deadline. A bounded send
            // would finish and release the lease while native I/O is still held.
            let premature = tokio::time::timeout(Duration::from_secs(6), &mut finished).await;
            let mut exclusive = Box::pin(
                shared
                    .receiver_access
                    .acquire_exclusive(ExclusiveAccessReason::Pairing),
            );
            let overtook = futures_lite::future::poll_once(&mut exclusive)
                .await
                .is_some();
            drop(exclusive);
            held.release();
            if premature.is_err() {
                tokio::time::timeout(Duration::from_secs(2), finished)
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert!(
                premature.is_err(),
                "stage {stage}: worker dropped an unfinished native report"
            );
            assert!(
                !overtook,
                "stage {stage}: receiver lease released before native completion"
            );
            assert_eq!(*keyboard.writes.lock().unwrap(), [0x81]);
        });
    }
    while let Some(result) = cases.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn cancelled_request_does_not_mutate_after_any_preflight_report() {
    for stage in 0..4 {
        let shared = orchestrator(Config::default()).shared();
        let keyboard = Keyboard::publish(&shared).await;
        shared
            .disabled_keys_order
            .sync_policy(&keyboard.route, Some(DisableKeysMask::EMPTY));
        let (refresh, abandoned) =
            crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
        shared.publish_inventory_refresh(refresh);
        let held = keyboard.hold_stage(stage);
        let mut request =
            Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
        assert!(
            futures_lite::future::poll_once(&mut request)
                .await
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
            .await
            .unwrap();
        drop(request);
        held.release();
        tokio::time::timeout(Duration::from_secs(2), abandoned)
            .await
            .unwrap();
        assert!(
            keyboard.writes.lock().unwrap().is_empty(),
            "stage {stage}: cancelled preflight submitted a mutation"
        );
    }
}

#[tokio::test]
async fn regression_cancelled_preflight_must_not_submit_a_mutation() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    shared
        .disabled_keys_order
        .sync_policy(&keyboard.route, Some(DisableKeysMask::EMPTY));
    let (refresh, abandoned) =
        crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
    shared.publish_inventory_refresh(refresh);
    let held = keyboard
        .reports
        .hold_next_write_completion(RequestMatch::Hidpp20, &[0x10, 0xff, 0, 0x10, 0, 0, 0]);
    let mut request =
        Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
    assert!(
        futures_lite::future::poll_once(&mut request)
            .await
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .unwrap();
    drop(request);
    held.release();
    tokio::time::timeout(Duration::from_secs(2), abandoned)
        .await
        .expect("cancelled worker drains");
    assert!(
        keyboard.writes.lock().unwrap().is_empty(),
        "cancellation during protocol discovery must prevent the later mutation"
    );
}

#[tokio::test]
async fn regression_native_query_timeout_must_retain_receiver_lease() {
    use crate::receiver_access::ExclusiveAccessReason;

    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    shared
        .disabled_keys_order
        .sync_policy(&keyboard.route, Some(DisableKeysMask::EMPTY));
    let (refresh, abandoned) =
        crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
    shared.publish_inventory_refresh(refresh);
    let held = keyboard
        .reports
        .hold_next_write_completion(RequestMatch::Hidpp20, &[0x10, 0xff, 0, 0x10, 0, 0, 0]);
    let mut request =
        Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
    assert!(
        futures_lite::future::poll_once(&mut request)
            .await
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .unwrap();
    // Wait for the requester's deadline to expire. The raw
    // completion barrier is deliberately still held; this is not a response wait.
    let outcome = tokio::time::timeout(Duration::from_secs(7), &mut request)
        .await
        .unwrap();
    assert!(
        outcome.is_err(),
        "held native query must not report success"
    );
    drop(request);
    let mut exclusive = Box::pin(
        shared
            .receiver_access
            .acquire_exclusive(ExclusiveAccessReason::Pairing),
    );
    let overtook = futures_lite::future::poll_once(&mut exclusive)
        .await
        .is_some();
    drop(exclusive);
    held.release();
    tokio::time::timeout(Duration::from_secs(2), abandoned)
        .await
        .expect("worker reports abandonment after native completion");
    assert!(
        !overtook,
        "receiver exclusive access overtook an unfinished native query"
    );
}

#[tokio::test]
async fn regression_reconciliation_read_must_not_overtake_queued_policy() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    shared
        .disabled_keys_order
        .sync_policy(&keyboard.route, Some(DisableKeysMask::CAPS_LOCK));
    let (_, ticket) = shared.disabled_keys_order.policy(&keyboard.route).unwrap();
    // Hold the existing setting owner to model a queued worker that has not
    // entered the transport yet. No production state is forged or reordered.
    let predecessor = ticket.turn().await.unwrap();
    let worker = shared.reapply_disabled_keys(&keyboard.route).unwrap();
    // Exact read path used by AgentServer::read_disable_keys.
    let mut read = Box::pin(shared.read_disable_keys(&keyboard.route));
    let early = tokio::time::timeout(Duration::from_millis(100), &mut read).await;
    drop(predecessor);
    tokio::task::spawn_blocking(move || worker.join().unwrap())
        .await
        .unwrap();
    let final_state = match early {
        Ok(state) => state.unwrap(),
        Err(_) => read.await.unwrap(),
    };
    assert_eq!(
        *keyboard.writes.lock().unwrap(),
        [0x81],
        "the queued policy did apply"
    );
    assert_eq!(
        final_state.disabled.bits(),
        0x81,
        "reconciliation returned the old mask before the policy worker applied the new one"
    );
}

#[tokio::test]
async fn cancelled_request_retains_native_write_turn_until_successor_completes() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    let old_write = keyboard.hold_set_write(0x81);
    let mut old = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
    assert!(
        futures_lite::future::poll_once(&mut old).await.is_none(),
        "the old requester starts its completion owner"
    );

    tokio::time::timeout(Duration::from_secs(2), old_write.write_accepted())
        .await
        .expect("old raw write reaches the transport");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut old)
            .await
            .is_err(),
        "the requester deadline must not finish the held native write"
    );
    drop(old);

    let successor_write = keyboard.hold_set_write(0x80);
    let mut successor = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(
        futures_lite::future::poll_once(&mut successor)
            .await
            .is_none(),
        "the successor starts its requester wait"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), successor_write.write_accepted())
            .await
            .is_err(),
        "the successor must not reach the raw transport before the old completion"
    );

    old_write.release();
    tokio::time::timeout(Duration::from_secs(2), successor_write.write_accepted())
        .await
        .expect("successor reaches the transport after old completion");
    successor_write.release();
    let state = tokio::time::timeout(Duration::from_secs(2), successor)
        .await
        .expect("successor completes")
        .expect("successor write succeeds");

    assert_eq!(
        state.disabled.bits(),
        0x80,
        "the successor owns final state"
    );
    assert_eq!(
        *keyboard.writes.lock().expect("writes"),
        [0x81, 0x80],
        "the old echo cannot replace the successor's final mask"
    );
}

#[tokio::test]
async fn queued_request_preserves_budget_until_native_predecessor_completes() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    let old_write = keyboard.hold_set_write(0x81);
    let mut old = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
    assert!(futures_lite::future::poll_once(&mut old).await.is_none());
    tokio::time::timeout(Duration::from_secs(2), old_write.write_accepted())
        .await
        .expect("predecessor enters native write");

    tokio::time::pause();
    let successor_write = keyboard.hold_set_write(0x80);
    let mut successor = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(
        futures_lite::future::poll_once(&mut successor)
            .await
            .is_none()
    );
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(
        futures_lite::future::poll_once(&mut successor)
            .await
            .is_none(),
        "queue wait must not exhaust the successor's five-second response budget"
    );
    tokio::time::resume();
    drop(old);
    old_write.release();
    tokio::time::timeout(Duration::from_secs(2), successor_write.write_accepted())
        .await
        .expect("successor can still start after the queue wait");
    successor_write.release();
    let state = tokio::time::timeout(Duration::from_secs(2), successor)
        .await
        .expect("successor completes")
        .expect("successor succeeds");
    assert_eq!(state.disabled.bits(), 0x80);
    assert_eq!(*keyboard.writes.lock().expect("writes"), [0x81, 0x80]);
}

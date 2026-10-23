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

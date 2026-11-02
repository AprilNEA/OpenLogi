use super::*;

use openlogi_agent_core::observable::ObservableState;
use openlogi_core::config::Config;
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};
use openlogi_device::replay::ReplayChannelHandle;
use openlogi_device::test_support::publish_scripted_channel;
use openlogi_fixture::RequestMatch;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn inventory(product_id: u16) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Synthetic backlight keyboard".into(),
            vendor_id: 0x046d,
            product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: 0xff,
            codename: Some("Synthetic backlight keyboard".into()),
            wpid: None,
            kind: DeviceKind::Keyboard,
            online: true,
            battery: None,
            model_info: None,
            capabilities: Some(Capabilities::default()),
            settings_revision: 0,
            hidpp10: false,
        }],
    }
}

struct Keyboard {
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: SharedHandles,
    observable: Arc<ObservableState>,
    route: DeviceRoute,
    reports: ReplayChannelHandle,
    fail_confirmation: Arc<AtomicBool>,
}

impl Keyboard {
    async fn new() -> Self {
        let observable = Arc::new(ObservableState::new("backlight-test".into()));
        let mut orchestrator = Orchestrator::new(Config::default(), observable.clone());
        orchestrator.refresh_inventory(&[inventory(0xb35b), inventory(0xb369)], &[], false);
        let shared = orchestrator.shared();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let enabled = StdMutex::new(true);
        let fail_confirmation = Arc::new(AtomicBool::new(false));
        let failure = fail_confirmation.clone();
        let wrote = AtomicBool::new(false);
        let (_, reports) = publish_scripted_channel(
            &shared.channel_registry,
            "backlight-test",
            route.clone(),
            move |request| {
                if request.len() < 7 {
                    return None;
                }
                let mut payload = [0; 16];
                let long = match (request[2], request[3] >> 4) {
                    (0, 1) => {
                        payload[0] = 4;
                        false
                    }
                    (0, 0) => {
                        if request[4..6] == [0x19, 0x82] {
                            payload[0] = 8;
                            payload[2] = 3;
                        }
                        false
                    }
                    (8, 0) => {
                        payload[0] = u8::from(*enabled.lock().unwrap());
                        payload[1] = 8;
                        payload[5] = 2;
                        !(wrote.load(Ordering::SeqCst) && failure.load(Ordering::SeqCst))
                    }
                    (8, 1) => {
                        *enabled.lock().unwrap() = request[4] != 0;
                        wrote.store(true, Ordering::SeqCst);
                        true
                    }
                    (8, 2) => {
                        payload[0] = 8;
                        payload[1] = 2;
                        true
                    }
                    _ => return None,
                };
                let mut response = vec![0; if long { 20 } else { 7 }];
                response[0] = if long { 0x11 } else { 0x10 };
                response[1..4].copy_from_slice(&request[1..4]);
                let size = response.len() - 4;
                response[4..].copy_from_slice(&payload[..size]);
                Some(response)
            },
        )
        .await;
        Self {
            orchestrator: Arc::new(Mutex::new(orchestrator)),
            shared,
            observable,
            route,
            reports,
            fail_confirmation,
        }
    }

    async fn set(&self, enabled: bool) -> Result<BacklightState, WriteError> {
        write(
            self.orchestrator.clone(),
            &self.shared,
            self.route.clone(),
            BacklightChange::Enabled(enabled),
        )
        .await
    }

    fn revisions(&self) -> Vec<u64> {
        self.observable
            .snapshot()
            .inventory
            .iter()
            .map(|inventory| inventory.paired[0].settings_revision)
            .collect()
    }

    fn written_values(&self) -> Vec<u8> {
        self.reports
            .written_reports()
            .iter()
            .filter(|report| report.len() >= 8 && report[2] == 8 && report[3] >> 4 == 1)
            .map(|report| report[4])
            .collect()
    }
}

#[tokio::test]
async fn a_backlight_write_notifies_other_clients_without_a_device_broadcast() {
    let keyboard = Keyboard::new().await;
    assert_eq!(keyboard.revisions(), [0, 0]);
    assert!(!keyboard.set(false).await.unwrap().enabled);
    assert_eq!(keyboard.written_values(), [0]);
    assert_eq!(keyboard.revisions(), [1, 0]);
    assert!(keyboard.set(true).await.unwrap().enabled);
    assert_eq!(keyboard.revisions(), [2, 0]);
    assert_eq!(keyboard.written_values(), [0, 1]);

    keyboard.orchestrator.lock().await.refresh_inventory(
        &[inventory(0xb35b), inventory(0xb369)],
        &[],
        false,
    );
    assert_eq!(
        keyboard.revisions(),
        [2, 0],
        "a normal scan cannot erase agent epochs"
    );
}

#[tokio::test]
async fn failed_backlight_confirmation_invalidates_without_claiming_success() {
    let keyboard = Keyboard::new().await;
    keyboard.fail_confirmation.store(true, Ordering::SeqCst);
    assert!(matches!(
        keyboard.set(false).await,
        Err(WriteError::UnsupportedResponse { .. })
    ));
    assert_eq!(
        keyboard.written_values(),
        [0],
        "an applied write can fail its readback"
    );
    assert_eq!(keyboard.revisions(), [1, 0]);
}

#[tokio::test]
async fn cancelling_the_backlight_requester_does_not_cancel_publication() {
    let keyboard = Keyboard::new().await;
    let held = keyboard.reports.hold_next_write_completion(
        RequestMatch::Hidpp20,
        &[
            0x11, 0xff, 8, 0x10, 0, 8, 0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    let mut observer = keyboard.observable.subscribe();
    let orchestrator = keyboard.orchestrator.clone();
    let shared = keyboard.shared.clone();
    let route = keyboard.route.clone();
    let requester = tokio::spawn(async move {
        write(
            orchestrator,
            &shared,
            route,
            BacklightChange::Enabled(false),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .unwrap();
    assert_eq!(
        keyboard.revisions(),
        [0, 0],
        "do not publish before the transaction settles"
    );
    requester.abort();
    assert!(requester.await.unwrap_err().is_cancelled());
    held.release();
    tokio::time::timeout(Duration::from_secs(2), observer.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(keyboard.revisions(), [1, 0]);
    assert_eq!(
        keyboard.written_values(),
        [0],
        "cancellation never retries the write"
    );
}

#[tokio::test]
async fn backlight_admission_order_does_not_depend_on_worker_poll_order() {
    let keyboard = Keyboard::new().await;
    let first = keyboard
        .shared
        .set_backlight(&keyboard.route, BacklightChange::Enabled(false));
    let mut second = Box::pin(
        keyboard
            .shared
            .set_backlight(&keyboard.route, BacklightChange::Enabled(true)),
    );
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert!(
        keyboard.reports.written_reports().is_empty(),
        "second admission waits before hardware I/O"
    );
    let (off, on) = tokio::join!(first, second);
    assert!(!off.unwrap().enabled);
    assert!(on.unwrap().enabled);
    assert_eq!(keyboard.written_values(), [0, 1]);
}

#[tokio::test]
async fn a_disappeared_backlight_route_is_not_resurrected_by_a_late_write() {
    let keyboard = Keyboard::new().await;
    let held = keyboard.reports.hold_next_write_completion(
        RequestMatch::Hidpp20,
        &[
            0x11, 0xff, 8, 0x10, 0, 8, 0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    let set = keyboard.set(false);
    tokio::pin!(set);
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = &mut set => panic!("held write finished: {result:?}"),
            () = held.write_accepted() => {},
        }
    })
    .await
    .unwrap();
    keyboard
        .orchestrator
        .lock()
        .await
        .refresh_inventory(&[inventory(0xb369)], &[], false);
    held.release();
    assert!(!set.await.unwrap().enabled);
    let snapshot = keyboard.observable.snapshot();
    assert_eq!(snapshot.inventory.len(), 1);
    assert_eq!(snapshot.inventory[0].receiver.product_id, 0xb369);
    assert_eq!(keyboard.revisions(), [0]);
}

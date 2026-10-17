//! How the light watcher schedules its watches. What a watched light reports
//! is covered at the device layer, next to the HID++ `0x1990` light fixture.
use std::sync::Arc;
use std::time::Duration;

use openlogi_hid::replay::{
    NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend, ReplayNode, ReplayTopology,
};
use openlogi_hid::{
    DeviceRoute, LITRA_GLOW_PRODUCT_ID, LitraModel, NodeId, NodeInfo, device_io_channel,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::{LightEvent, ManagerCompletion, WatchedLight, manage};
use crate::hardware::HardwareContext;

/// A manager running in the test's runtime, so paused time drives its
/// retries.
struct Running {
    backend: Arc<ReplayBackend>,
    node: NodeId,
    light: WatchedLight,
    lights: watch::Sender<Arc<Vec<WatchedLight>>>,
    events: mpsc::UnboundedReceiver<LightEvent>,
    stop: oneshot::Sender<()>,
    manager: JoinHandle<ManagerCompletion>,
}

impl Running {
    /// Watch one Litra Glow whose HID++ open has `outcome`. `serial` must be
    /// unique per test: the raw-only answer is remembered per node.
    fn start(serial: &str, outcome: OpenOutcome) -> Self {
        let node = NodeId::from(format!("light-watcher-{serial}"));
        let topology = ReplayTopology {
            nodes: vec![ReplayNode {
                info: NodeInfo {
                    id: node.clone(),
                    vendor_id: 0x046d,
                    product_id: LITRA_GLOW_PRODUCT_ID,
                    usage_page: 0xff43,
                    usage_id: 0x0202,
                    name: "Watched Litra Glow".to_string(),
                    manufacturer: Some("Logitech".to_string()),
                    serial_number: Some(serial.to_string()),
                },
                presence: NodePresence::Present,
                open_outcome: outcome,
                channel: None,
                raw_writer: RawWriterAvailability::Capture,
                receiver_slots: Vec::new(),
            }],
            channels: Vec::new(),
        };
        let backend = Arc::new(
            ReplayBackend::new(topology, Vec::new()).expect("valid Litra replay topology"),
        );
        let light = WatchedLight {
            route: DeviceRoute::RawHid {
                vendor_id: 0x046d,
                product_id: LITRA_GLOW_PRODUCT_ID,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: format!("serial:{}", serial.to_ascii_lowercase()),
            },
            model: LitraModel::Glow,
        };
        let (_, device_io) = device_io_channel();
        let hardware = HardwareContext::injected(backend.clone(), device_io);
        let (lights, wanted) = watch::channel(Arc::new(vec![light.clone()]));
        let (events_tx, events) = mpsc::unbounded_channel();
        let (stop, shutdown) = oneshot::channel();
        let manager = tokio::spawn(manage(hardware, wanted, events_tx, shutdown));
        Self {
            backend,
            node,
            light,
            lights,
            events,
            stop,
            manager,
        }
    }

    fn opens(&self) -> usize {
        self.backend
            .open_count(&self.node)
            .expect("known Litra node")
    }

    fn publish(&self, lights: Vec<WatchedLight>) {
        self.lights.send_replace(Arc::new(lights));
    }

    /// Stop the manager. None of these lights answers over HID++, so none
    /// may have reported anything.
    async fn stop(mut self) {
        let _ = self.stop.send(());
        let completion = self.manager.await.expect("the manager does not panic");
        assert!(matches!(completion, ManagerCompletion::Graceful));
        if let Ok(event) = self.events.try_recv() {
            panic!("a light that is not watched over HID++ reported {event:?}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_light_reached_only_raw_is_asked_once() {
    let running = Running::start("WATCH-RAW", OpenOutcome::NotHidpp);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(running.opens(), 1);

    // The same light published again is not asked again.
    running.publish(vec![running.light.clone()]);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(running.opens(), 1);

    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_light_is_watched_again_until_it_goes_away() {
    let running = Running::start("WATCH-DENIED", OpenOutcome::Denied);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(running.opens(), 1);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        running.opens(),
        2,
        "a refused open is tried again after 5 s"
    );

    running.publish(Vec::new());
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(running.opens(), 2, "a light that went away is not watched");

    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_light_that_comes_back_is_watched_again() {
    let running = Running::start("WATCH-BACK", OpenOutcome::Denied);
    tokio::time::sleep(Duration::from_secs(1)).await;
    running.publish(Vec::new());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(running.opens(), 1);

    running.publish(vec![running.light.clone()]);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(running.opens(), 2);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_manager_stops_when_the_orchestrator_is_gone() {
    let Running {
        backend: _backend,
        lights,
        stop,
        manager,
        ..
    } = Running::start("WATCH-GONE", OpenOutcome::Denied);
    drop(lights);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !manager.is_finished(),
        "the manager waits for its stop request"
    );
    let _ = stop.send(());
    let completion = manager.await.expect("the manager does not panic");
    assert!(matches!(completion, ManagerCompletion::Graceful));
}

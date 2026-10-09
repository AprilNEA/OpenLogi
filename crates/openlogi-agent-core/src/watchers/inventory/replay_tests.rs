//! Replay-backed vertical coverage for the agent hardware boundary.

use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use openlogi_hid::backend::HotplugEvent;
use openlogi_hid::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};
use openlogi_hid::{
    ChannelRegistry, DeviceRoute, Dpi, HidppOperation, NodeId, NodeInfo, device_io_channel,
    get_dpi_info_on,
};

use crate::hardware::HardwareContext;
use crate::observable::ObservableState;
use crate::orchestrator::Orchestrator;

use super::{InventoryEvent, InventoryWatcher, spawn_with_hardware};

const CHANNEL: &str = "agent-direct-mouse";
const PRODUCT_ID: u16 = 0xb35b;

#[tokio::test]
async fn replay_inventory_and_authoritative_read_share_one_injected_backend() {
    let node_id = NodeId::from("agent-direct-node".to_string());
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: PRODUCT_ID,
    };
    let backend = Arc::new(
        ReplayBackend::new(
            direct_topology(node_id.clone()),
            vec![direct_inventory_and_dpi_cassette()],
        )
        .expect("valid agent replay fixture"),
    );
    let (_device_io_signal, device_io) = device_io_channel();
    let hardware = HardwareContext::injected(backend.clone(), device_io);
    let observable = Arc::new(ObservableState::new("test".to_string()));
    let mut orchestrator = Orchestrator::with_hardware(Config::default(), observable, hardware);
    let shared = orchestrator.shared();
    let mut watcher = spawn_with_hardware(shared.hardware(), shared.channel_registry.clone());

    let event = tokio::time::timeout(Duration::from_secs(2), watcher.events.recv())
        .await
        .expect("initial replay inventory must be bounded")
        .expect("inventory watcher must publish its initial snapshot");
    let (inventories, standalone, hid_open_failures) = match event {
        InventoryEvent::Snapshot {
            inventories,
            standalone,
            hid_open_failures,
        } => (inventories, standalone, hid_open_failures),
        InventoryEvent::Unavailable | InventoryEvent::SystemWake => {
            panic!("initial replay reconciliation must publish a snapshot")
        }
    };
    assert_eq!(inventories.len(), 1);
    assert!(standalone.is_empty());
    assert!(!hid_open_failures);
    assert!(
        shared
            .channel_registry
            .lookup(&route)
            .is_some_and(|channel| channel.matches(&route)),
        "the watcher must publish the exact direct-device route"
    );
    assert!(
        shared
            .channel_registry
            .lookup(&DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: PRODUCT_ID + 1,
            })
            .is_none(),
        "registry lookup must remain exact"
    );

    orchestrator.refresh_inventory(&inventories, &standalone, hid_open_failures);
    assert_eq!(orchestrator.inventory(), inventories);

    let dpi = shared
        .device(&route)
        .run(HidppOperation::ReadDpiCapabilities, |channel| async move {
            get_dpi_info_on(&channel).await
        })
        .await
        .expect("authoritative replay DPI read succeeds");
    assert_eq!(dpi.current, Dpi::new(800));
    assert_eq!(
        dpi.capabilities.values(),
        [Dpi::new(400), Dpi::new(800), Dpi::new(1600)]
    );
    assert_eq!(backend.open_count(&node_id).expect("known replay node"), 1);
    let completion = backend
        .channel_completion(CHANNEL)
        .expect("known replay channel");
    assert_eq!(completion.channel_open_count, 1);
    assert!(completion.unmatched_requests.is_empty());
    backend
        .require_complete()
        .expect("inventory and DPI exchanges fully consumed");
}

fn direct_topology(node_id: NodeId) -> ReplayTopology {
    ReplayTopology {
        nodes: vec![ReplayNode {
            info: NodeInfo {
                id: node_id,
                vendor_id: 0x046d,
                product_id: PRODUCT_ID,
                usage_page: 0xff00,
                usage_id: 0x0002,
                name: "Agent Replay Mouse".to_string(),
                manufacturer: Some("Logitech".to_string()),
                serial_number: None,
            },
            presence: NodePresence::Present,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(CHANNEL.to_string()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: Vec::new(),
        }],
        channels: vec![ReplayChannel {
            id: CHANNEL.to_string(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

fn direct_inventory_and_dpi_cassette() -> HidCassette {
    let mut exchanges = direct_probe_exchanges();
    exchanges.extend([
        h20(
            short(0xff, 0x00, 0x10, [0, 0, 0]),
            short(0xff, 0x00, 0x10, [4, 0, 0]),
        ),
        h20(
            short(0xff, 0x00, 0x00, [0x22, 0x01, 0]),
            short(0xff, 0x00, 0x00, [0x02, 0, 0]),
        ),
        h20(
            short(0xff, 0x02, 0x00, [0, 0, 0]),
            short(0xff, 0x02, 0x00, [1, 0, 0]),
        ),
        h20(
            short(0xff, 0x02, 0x20, [0, 0, 0]),
            short(0xff, 0x02, 0x20, [0, 0x03, 0x20]),
        ),
        h20(
            short(0xff, 0x02, 0x10, [0, 0, 0]),
            long(
                0xff,
                0x02,
                0x10,
                [
                    0, 0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
            ),
        ),
    ]);
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "agent inventory and DPI read".to_string(),
        channel: CHANNEL.to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges,
    }
}

/// The feature walk one inventory probe of the replay mouse performs.
fn direct_probe_exchanges() -> Vec<CassetteExchange> {
    vec![
        h20(
            short(0xff, 0x00, 0x10, [0, 0, 0]),
            short(0xff, 0x00, 0x10, [4, 0, 0]),
        ),
        h20(
            short(0xff, 0x00, 0x00, [0x00, 0x01, 0]),
            short(0xff, 0x00, 0x00, [0x01, 0, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x00, [0, 0, 0]),
            short(0xff, 0x01, 0x00, [2, 0, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x10, [1, 0, 0]),
            short(0xff, 0x01, 0x10, [0x00, 0x01, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x10, [2, 0, 0]),
            short(0xff, 0x01, 0x10, [0x22, 0x01, 0]),
        ),
    ]
}

/// A Bluetooth-direct mouse across one idle sleep. macOS tears down a BLE
/// peripheral's HID node when the link drops and publishes a new one, under a
/// new registry id, when it reconnects; both lifetimes speak for the same
/// logical channel, so the cassette holds one probe for each.
struct SleepingMouse {
    backend: Arc<ReplayBackend>,
    before_sleep: NodeId,
    after_wake: NodeId,
}

impl SleepingMouse {
    fn new() -> Self {
        let before_sleep = NodeId::from("agent-ble-mouse-before-sleep".to_string());
        let after_wake = NodeId::from("agent-ble-mouse-after-wake".to_string());
        let mut topology = direct_topology(before_sleep.clone());
        let mut woken = topology.nodes[0].clone();
        woken.info.id = after_wake.clone();
        woken.presence = NodePresence::Absent;
        topology.nodes.push(woken);
        let mut exchanges = direct_probe_exchanges();
        exchanges.extend(direct_probe_exchanges());
        let cassette = HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: "agent BLE mouse across an idle sleep".to_string(),
            channel: CHANNEL.to_string(),
            report_support: ReportSupport::ShortAndLong,
            exchanges,
        };
        let backend = Arc::new(
            ReplayBackend::new(topology, vec![cassette]).expect("valid sleeping-mouse replay"),
        );
        Self {
            backend,
            before_sleep,
            after_wake,
        }
    }

    /// The link drops: the old node leaves the OS tree and its open handle
    /// goes dead.
    fn sleep(&self) {
        self.backend
            .set_node_presence(&self.before_sleep, NodePresence::Absent)
            .expect("known node");
        self.backend
            .set_channel_connection(CHANNEL, ChannelConnection::Disconnected)
            .expect("known channel");
        self.backend.emit_hotplug(HotplugEvent::Disconnected);
    }

    /// The link comes back as a new node.
    fn wake(&self) {
        self.backend
            .set_channel_connection(CHANNEL, ChannelConnection::Connected)
            .expect("known channel");
        self.backend
            .set_node_presence(&self.after_wake, NodePresence::Present)
            .expect("known node");
        self.backend.emit_hotplug(HotplugEvent::Connected);
    }
}

/// Long enough for a hotplug-triggered reconciliation to settle and publish.
const PUBLISH_WINDOW: Duration = Duration::from_secs(5);

/// Shorter than [`PUBLISH_WINDOW`], still well past the watcher's settle
/// delays: a reconciliation the gate failed to hold would publish inside it.
const PAUSED_WINDOW: Duration = Duration::from_secs(1);

/// The first inventory snapshot within `window` that satisfies `wanted`.
/// Repair retries publish their own snapshots, so the one a test waits for is
/// not necessarily the next one.
async fn published_within(
    watcher: &mut InventoryWatcher,
    window: Duration,
    wanted: impl Fn(&[DeviceInventory]) -> bool,
) -> Option<Vec<DeviceInventory>> {
    tokio::time::timeout(window, async {
        loop {
            match watcher.events.recv().await {
                Some(InventoryEvent::Snapshot { inventories, .. }) if wanted(&inventories) => {
                    return inventories;
                }
                Some(InventoryEvent::Snapshot { .. } | InventoryEvent::SystemWake) => {}
                Some(InventoryEvent::Unavailable) => {
                    panic!("a replay backend never makes enumeration unavailable")
                }
                None => panic!("the inventory watcher stopped"),
            }
        }
    })
    .await
    .ok()
}

async fn published(
    watcher: &mut InventoryWatcher,
    wanted: impl Fn(&[DeviceInventory]) -> bool,
) -> Vec<DeviceInventory> {
    published_within(watcher, PUBLISH_WINDOW, wanted)
        .await
        .expect("the wanted inventory snapshot must arrive")
}

fn has_devices(inventories: &[DeviceInventory]) -> bool {
    !inventories.is_empty()
}

fn direct_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: PRODUCT_ID,
    }
}

#[tokio::test]
async fn a_bluetooth_mouse_that_wakes_as_a_new_node_is_published_again() {
    let mouse = SleepingMouse::new();
    let (_device_io_signal, device_io) = device_io_channel();
    let registry = ChannelRegistry::default();
    let mut watcher = spawn_with_hardware(
        HardwareContext::injected(mouse.backend.clone(), device_io),
        registry.clone(),
    );
    let awake = published(&mut watcher, has_devices).await;
    // A capture session still holds the channel when the link drops; the
    // inventory must not need it released to publish the absence.
    let capture = registry
        .lookup(&direct_route())
        .expect("the awake mouse is published");

    mouse.sleep();
    published(&mut watcher, <[DeviceInventory]>::is_empty).await;
    assert!(registry.lookup(&direct_route()).is_none());
    drop(capture);

    mouse.wake();
    let woken = published(&mut watcher, has_devices).await;

    assert_eq!(woken, awake);
    assert!(
        registry.lookup(&direct_route()).is_some(),
        "capture must find a channel for the woken mouse"
    );
    assert_eq!(
        mouse
            .backend
            .open_count(&mouse.after_wake)
            .expect("known node"),
        1,
        "the published channel is the woken node's own open"
    );
}

#[tokio::test]
async fn a_mouse_that_wakes_while_device_io_is_paused_is_published_on_resume() {
    let mouse = SleepingMouse::new();
    let (device_io_signal, device_io) = device_io_channel();
    let registry = ChannelRegistry::default();
    let mut watcher = spawn_with_hardware(
        HardwareContext::injected(mouse.backend.clone(), device_io),
        registry.clone(),
    );
    let awake = published(&mut watcher, has_devices).await;
    mouse.sleep();
    published(&mut watcher, <[DeviceInventory]>::is_empty).await;

    // The mouse wakes while device I/O is paused: the inventory keeps serving
    // its last snapshot, empty here, until the gate reopens.
    assert!(device_io_signal.suspend());
    mouse.wake();
    assert!(
        published_within(&mut watcher, PAUSED_WINDOW, has_devices)
            .await
            .is_none(),
        "a paused gate must not publish the woken mouse"
    );
    assert_eq!(
        mouse
            .backend
            .open_count(&mouse.after_wake)
            .expect("known node"),
        0,
        "a paused gate must not open the woken mouse either"
    );
    assert!(device_io_signal.resume());
    let woken = published(&mut watcher, has_devices).await;

    assert_eq!(woken, awake);
    assert!(registry.lookup(&direct_route()).is_some());
}

fn h20(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}

fn long(device: u8, feature: u8, function: u8, payload: [u8; 16]) -> Vec<u8> {
    let mut report = vec![0x11, device, feature, function];
    report.extend_from_slice(&payload);
    report
}

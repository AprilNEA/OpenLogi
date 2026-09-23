use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use openlogi_core::binding::ButtonId;
use openlogi_core::hid::PairingError;
use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use tokio::sync::{mpsc, oneshot};

use super::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayResponseBarrier, ReplayTopology,
};
use crate::session::gesture::CaptureSpec;
use crate::{
    CaptureChannelSlot, CaptureHost, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput,
    ChannelRegistry, DeviceIoGate, DeviceIoSignal, DeviceRoute, Enumerator, NodeId, NodeInfo,
    PairingCommand, PairingEvent, ReceiverSelector, SharedChannel, device_io_channel,
    reprog_controls, run_capture_session, run_keyboard_capture_session, run_pairing,
};

const CAPTURE_CHANNEL: &str = "capture-session";
const PAIRING_CHANNEL: &str = "bolt-pairing-session";
const DIRECT_PRODUCT_ID: u16 = 0xb35b;
const BOLT_PRODUCT_ID: u16 = 0xc548;
const REPROG_FEATURE_INDEX: u8 = 0x02;
const GESTURE_CID: u16 = reprog_controls::GESTURE_BUTTON_CID;
/// The Mute key, one of [`crate::KEYBOARD_KEY_CIDS`].
const KEYBOARD_CID: u16 = 0x00e7;
const ORIGINAL_REMAP_CID: u16 = 0x0053;
/// A `getCidInfo` capability pair (bytes 4 and 8): a mouse control that is
/// reprogrammable and divertable, with raw XY.
const GESTURE_CONTROL_FLAGS: (u8, u8) = (0x31, 0x01);
/// A function-row control that is reprogrammable and divertable, no raw XY.
const KEYBOARD_CONTROL_FLAGS: (u8, u8) = (0x32, 0x00);
/// `setCidReporting` flags that arm gesture capture: divert and raw XY, both
/// marked valid.
const GESTURE_ARMED_FLAGS: u8 = 0x33;
/// `setCidReporting` flags that arm keyboard capture: divert marked valid, raw
/// XY marked valid but off.
const KEYBOARD_ARMED_FLAGS: u8 = 0x23;
/// `setCidReporting` flags that hand a control back: divert and raw XY both
/// marked valid and off.
const RESTORED_FLAGS: u8 = 0x22;

#[tokio::test]
async fn gesture_capture_replay_restores_original_reporting_on_normal_shutdown() {
    let replay = ArmedReplay::enumerate(capture_cassette(
        "gesture capture normal shutdown",
        GESTURE_CID,
        GESTURE_CONTROL_FLAGS,
        GESTURE_ARMED_FLAGS,
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_capture_session(
        replay.route.clone(),
        CaptureSpec {
            divert_gesture_sources: vec![GESTURE_CID],
            ..CaptureSpec::default()
        },
        host,
    );

    let (outcome, ()) = tokio::join!(capture, replay.stop_after_arm(shutdown));

    replay.assert_restored(outcome, GESTURE_CID, GESTURE_ARMED_FLAGS);
}

/// Keyboard capture arms its own way — `0x1b04` diversion on the wanted
/// controls only, without raw XY — and then runs the skeleton the gesture
/// session runs, so it has to publish its channel before it monitors and hand
/// the control back before it completes, exactly as that session does.
#[tokio::test]
async fn keyboard_capture_replay_restores_original_reporting_on_normal_shutdown() {
    let replay = ArmedReplay::enumerate(capture_cassette(
        "keyboard capture normal shutdown",
        KEYBOARD_CID,
        KEYBOARD_CONTROL_FLAGS,
        KEYBOARD_ARMED_FLAGS,
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_keyboard_capture_session(
        replay.route.clone(),
        BTreeMap::from([(KEYBOARD_CID, ButtonId::KeyMute)]),
        host,
    );

    let (outcome, ()) = tokio::join!(capture, replay.stop_after_arm(shutdown));

    replay.assert_restored(outcome, KEYBOARD_CID, KEYBOARD_ARMED_FLAGS);
}

/// One replayed direct device, enumerated the production way, whose `0x1d4b`
/// lookup is held so a capture session can be observed between arming and
/// monitoring.
struct ArmedReplay {
    backend: Arc<ReplayBackend>,
    registry: ChannelRegistry,
    node_id: NodeId,
    route: DeviceRoute,
    original_publication: SharedChannel,
    channel_slot: CaptureChannelSlot,
    wireless_lookup: ReplayResponseBarrier,
    /// Held so the gate stays open for the session's lifetime.
    _io_signal: DeviceIoSignal,
    io_gate: DeviceIoGate,
}

impl ArmedReplay {
    async fn enumerate(cassette: HidCassette) -> Self {
        let node_id = NodeId::from("capture-node".to_string());
        let route = DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id: DIRECT_PRODUCT_ID,
        };
        let backend = Arc::new(
            ReplayBackend::new(
                replay_topology(direct_capture_node(node_id.clone()), CAPTURE_CHANNEL),
                vec![cassette],
            )
            .expect("capture replay topology is valid"),
        );
        let registry = ChannelRegistry::default();
        let mut enumerator =
            Enumerator::with_backend(backend.clone()).with_registry(registry.clone());

        let inventory = enumerator
            .enumerate()
            .await
            .expect("production feature probe succeeds");
        assert_eq!(inventory.len(), 1);
        let original_publication = registry
            .lookup(&route)
            .expect("the enumerator publishes the direct route");
        assert!(registry.is_current(&original_publication));
        assert_eq!(
            backend
                .channel_lifetime_count(CAPTURE_CHANNEL)
                .expect("known capture channel"),
            1
        );

        let wireless_lookup = backend
            .hold_next_response(
                CAPTURE_CHANNEL,
                RequestMatch::Hidpp20,
                &root_feature_lookup_request(0x1d4b),
            )
            .expect("wireless feature lookup can be held");
        let (io_signal, io_gate) = device_io_channel();
        Self {
            backend,
            registry,
            node_id,
            route,
            original_publication,
            channel_slot: Arc::new(RwLock::new(None)),
            wireless_lookup,
            _io_signal: io_signal,
            io_gate,
        }
    }

    /// The host handles one session runs against, and the sender that shuts
    /// it down.
    fn host(
        &self,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> (oneshot::Sender<()>, CaptureHost<'_>) {
        let (shutdown, shutdown_rx) = oneshot::channel();
        let host = CaptureHost {
            sink,
            shutdown: shutdown_rx,
            channel_slot: Arc::clone(&self.channel_slot),
            registry: &self.registry,
            device_io: self.io_gate.clone(),
        };
        (shutdown, host)
    }

    /// Once the session has armed and asked for `0x1d4b`, check that it has
    /// published its channel, then shut it down and let the lookup answer.
    async fn stop_after_arm(&self, shutdown: oneshot::Sender<()>) {
        self.wireless_lookup.request_written().await;
        let published = self
            .channel_slot
            .read()
            .expect("capture channel slot is readable")
            .clone()
            .expect("capture channel is published before the wireless lookup");
        assert!(self.registry.is_current(&published));
        shutdown
            .send(())
            .expect("capture session still owns its shutdown receiver");
        self.wireless_lookup.release();
    }

    /// A normal shutdown restored `cid`'s reporting through the channel the
    /// enumerator owns, cleared the slot, and consumed the whole cassette.
    fn assert_restored(
        &self,
        outcome: Result<CaptureSessionOutcome, CaptureSessionFailure>,
        cid: u16,
        armed_flags: u8,
    ) {
        assert!(matches!(
            outcome.expect("capture session shuts down cleanly"),
            CaptureSessionOutcome::Restored
        ));
        assert!(
            self.channel_slot
                .read()
                .expect("capture channel slot is readable")
                .is_none(),
            "normal shutdown must clear the captured channel slot"
        );
        assert!(
            self.registry.is_current(&self.original_publication),
            "normal shutdown must restore through and retain the original publication"
        );
        assert_eq!(
            self.backend
                .open_count(&self.node_id)
                .expect("known capture node"),
            1
        );
        assert_eq!(
            self.backend
                .channel_lifetime_count(CAPTURE_CHANNEL)
                .expect("known capture channel"),
            1,
            "capture must reuse the enumerator-owned channel"
        );

        let completion = self
            .backend
            .channel_completion(CAPTURE_CHANNEL)
            .expect("known capture channel");
        let reporting_writes: Vec<_> = completion
            .written_reports
            .iter()
            .filter(|report| {
                report[0] == 0x11 && report[2] == REPROG_FEATURE_INDEX && report[3] >> 4 == 3
            })
            .collect();
        assert_eq!(reporting_writes.len(), 2);
        assert_eq!(
            &reporting_writes[0][4..],
            &reprog_reporting_change_payload(cid, armed_flags),
            "arming sets only the diversion this capture needs while preserving the original remap"
        );
        assert_eq!(
            &reporting_writes[1][4..],
            &reprog_reporting_change_payload(cid, RESTORED_FLAGS),
            "shutdown clears only diversion and raw-XY while preserving the original remap"
        );
        assert_eq!(completion.channel_open_count, 1);
        self.backend
            .require_complete()
            .expect("capture cassette is strictly consumed");
    }
}

#[tokio::test]
async fn bolt_pairing_replay_cancels_discovery_and_restores_notifications() {
    let node_id = NodeId::from("bolt-pairing-node".to_string());
    let backend = ReplayBackend::new(
        replay_topology(bolt_pairing_node(node_id.clone()), PAIRING_CHANNEL),
        vec![bolt_pairing_cancel_cassette()],
    )
    .expect("pairing replay topology is valid");
    let (command_tx, commands) = mpsc::unbounded_channel();
    let (event_tx, mut events) = mpsc::unbounded_channel();

    let pairing = run_pairing(&backend, ReceiverSelector::First, commands, event_tx);
    let cancel_after_searching = async {
        let searching = events
            .recv()
            .await
            .expect("pairing emits its searching phase");
        assert!(matches!(searching, PairingEvent::Searching));
        command_tx
            .send(PairingCommand::Cancel)
            .expect("the searching session accepts cancellation");

        let terminal = events.recv().await.expect("pairing emits a terminal event");
        assert!(matches!(
            terminal,
            PairingEvent::Failed(PairingError::Cancelled)
        ));
        assert!(
            events.recv().await.is_none(),
            "searching and failed must be the complete event sequence"
        );
    };

    let (result, ()) = tokio::join!(pairing, cancel_after_searching);
    assert!(matches!(result, Err(PairingError::Cancelled)));
    assert_eq!(backend.open_count(&node_id).expect("known pairing node"), 1);

    let completion = backend
        .channel_completion(PAIRING_CHANNEL)
        .expect("known pairing channel");
    assert_eq!(
        completion.written_reports,
        vec![
            receiver_notification_flags_write([0x00, 0x09, 0x00]),
            bolt_discovery_write([30, 0x01, 0x00]),
            bolt_discovery_write([30, 0x02, 0x00]),
            receiver_notification_flags_write([0x00, 0x00, 0x00]),
        ],
        "cancel while searching must stop Bolt discovery before restoring notification flags"
    );
    assert_eq!(completion.channel_open_count, 1);
    assert_eq!(
        backend
            .channel_lifetime_count(PAIRING_CHANNEL)
            .expect("known pairing channel"),
        0,
        "the pairing receiver channel closes when the session returns"
    );
    backend
        .require_complete()
        .expect("pairing cassette is strictly consumed");
}

fn direct_capture_node(id: NodeId) -> ReplayNode {
    replay_node(id, DIRECT_PRODUCT_ID, "Capture Device")
}

fn bolt_pairing_node(id: NodeId) -> ReplayNode {
    replay_node(id, BOLT_PRODUCT_ID, "Logi Bolt Receiver")
}

fn replay_node(id: NodeId, product_id: u16, name: &str) -> ReplayNode {
    ReplayNode {
        info: NodeInfo {
            id,
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id,
            usage_page: 0xff00,
            usage_id: 0x0002,
            name: name.to_string(),
            manufacturer: Some("Logitech".to_string()),
            serial_number: None,
        },
        presence: NodePresence::Present,
        open_outcome: OpenOutcome::Hidpp,
        channel: Some(if product_id == BOLT_PRODUCT_ID {
            PAIRING_CHANNEL.to_string()
        } else {
            CAPTURE_CHANNEL.to_string()
        }),
        raw_writer: RawWriterAvailability::Unavailable,
        receiver_slots: Vec::new(),
    }
}

fn replay_topology(node: ReplayNode, channel: &str) -> ReplayTopology {
    ReplayTopology {
        nodes: vec![node],
        channels: vec![ReplayChannel {
            id: channel.to_string(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

/// The exchanges one capture session over `cid` costs, from the production
/// probe through arming, the held `0x1d4b` lookup, and the restore on shutdown.
fn capture_cassette(name: &str, cid: u16, control_flags: (u8, u8), armed_flags: u8) -> HidCassette {
    cassette(
        name,
        CAPTURE_CHANNEL,
        vec![
            root_ping_exchange(),
            root_feature_lookup_exchange(0x0001, 0x01, 0),
            feature_set_count_exchange(2),
            feature_set_entry_exchange(1, 0x0001),
            feature_set_entry_exchange(2, reprog_controls::FEATURE_ID),
            reprog_control_count_exchange(1),
            reprog_control_info_exchange(cid, control_flags),
            root_ping_exchange(),
            root_feature_lookup_exchange(reprog_controls::FEATURE_ID, REPROG_FEATURE_INDEX, 4),
            reprog_control_count_exchange(1),
            reprog_control_info_exchange(cid, control_flags),
            reprog_reporting_state_exchange(cid),
            reprog_reporting_change_exchange(cid, armed_flags),
            root_feature_lookup_exchange(0x1d4b, 0, 0),
            reprog_reporting_change_exchange(cid, RESTORED_FLAGS),
        ],
    )
}

fn bolt_pairing_cancel_cassette() -> HidCassette {
    cassette(
        "Bolt discovery cancellation",
        PAIRING_CHANNEL,
        [
            receiver_notification_flags_write([0x00, 0x09, 0x00]),
            bolt_discovery_write([30, 0x01, 0x00]),
            bolt_discovery_write([30, 0x02, 0x00]),
            receiver_notification_flags_write([0x00, 0x00, 0x00]),
        ]
        .into_iter()
        .map(exact_echo_exchange)
        .collect(),
    )
}

fn cassette(name: &str, channel: &str, exchanges: Vec<CassetteExchange>) -> HidCassette {
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: name.to_string(),
        channel: channel.to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges,
    }
}

fn root_ping_exchange() -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, 0x00, 1, [0, 0, 0]),
        hidpp20_short(0xff, 0x00, 1, [4, 0, 0]),
    )
}

fn root_feature_lookup_request(feature_id: u16) -> Vec<u8> {
    let [high, low] = feature_id.to_be_bytes();
    hidpp20_short(0xff, 0x00, 0, [high, low, 0])
}

fn root_feature_lookup_exchange(
    feature_id: u16,
    feature_index: u8,
    version: u8,
) -> CassetteExchange {
    hidpp20_exchange(
        root_feature_lookup_request(feature_id),
        hidpp20_short(0xff, 0x00, 0, [feature_index, 0, version]),
    )
}

fn feature_set_count_exchange(count: u8) -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, 0x01, 0, [0, 0, 0]),
        hidpp20_short(0xff, 0x01, 0, [count, 0, 0]),
    )
}

fn feature_set_entry_exchange(index: u8, feature_id: u16) -> CassetteExchange {
    let [high, low] = feature_id.to_be_bytes();
    hidpp20_exchange(
        hidpp20_short(0xff, 0x01, 1, [index, 0, 0]),
        hidpp20_short(0xff, 0x01, 1, [high, low, 0]),
    )
}

fn reprog_control_count_exchange(count: u8) -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 0, [0, 0, 0]),
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 0, [count, 0, 0]),
    )
}

fn reprog_control_info_exchange(cid: u16, (flags, flags_high): (u8, u8)) -> CassetteExchange {
    let mut response = [0u8; 16];
    response[0..2].copy_from_slice(&cid.to_be_bytes());
    response[2..4].copy_from_slice(&0x009cu16.to_be_bytes());
    response[4] = flags;
    response[8] = flags_high;
    hidpp20_exchange(
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 1, [0; 16]),
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 1, response),
    )
}

fn reprog_reporting_state_exchange(cid: u16) -> CassetteExchange {
    let [cid_high, cid_low] = cid.to_be_bytes();
    let mut response = [0u8; 16];
    response[0..2].copy_from_slice(&cid.to_be_bytes());
    response[2] = 0x44;
    response[3..5].copy_from_slice(&ORIGINAL_REMAP_CID.to_be_bytes());
    response[5] = 0x05;
    hidpp20_exchange(
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 2, [cid_high, cid_low, 0]),
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 2, response),
    )
}

fn reprog_reporting_change_exchange(cid: u16, flags: u8) -> CassetteExchange {
    let payload = reprog_reporting_change_payload(cid, flags);
    let report = hidpp20_long(0xff, REPROG_FEATURE_INDEX, 3, payload);
    hidpp20_exchange(report.clone(), report)
}

fn reprog_reporting_change_payload(cid: u16, flags: u8) -> [u8; 16] {
    let mut payload = [0u8; 16];
    payload[0..2].copy_from_slice(&cid.to_be_bytes());
    payload[2] = flags;
    payload[3..5].copy_from_slice(&ORIGINAL_REMAP_CID.to_be_bytes());
    payload
}

fn receiver_notification_flags_write(flags: [u8; 3]) -> Vec<u8> {
    hidpp10_register_write(0x00, flags)
}

fn bolt_discovery_write(payload: [u8; 3]) -> Vec<u8> {
    hidpp10_register_write(0xc0, payload)
}

fn hidpp10_register_write(address: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, 0xff, 0x80, address, payload[0], payload[1], payload[2],
    ]
}

fn exact_echo_exchange(report: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Exact,
        request: report.clone(),
        response: Some(report),
        required: true,
    }
}

fn hidpp20_exchange(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

fn hidpp20_short(device_index: u8, feature_index: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10,
        device_index,
        feature_index,
        function << 4,
        payload[0],
        payload[1],
        payload[2],
    ]
}

fn hidpp20_long(device_index: u8, feature_index: u8, function: u8, payload: [u8; 16]) -> Vec<u8> {
    let mut report = vec![0x11, device_index, feature_index, function << 4];
    report.extend_from_slice(&payload);
    report
}

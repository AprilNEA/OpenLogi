use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};

use crate::fixture::{
    CassetteExchange, ChannelConnection, FIXTURE_SCHEMA_VERSION, HidCassette, NodePresence,
    OpenOutcome, RawWriterAvailability, ReceiverLinkState, ReceiverSlot, ReceiverSlotState,
    ReplayChannel, ReplayNode, ReportSupport, RequestMatch,
};
use crate::{DeviceRoute, NodeId, NodeInfo};

pub(super) const DIRECT_CHANNEL: &str = "scenario-direct";
pub(super) const BOLT_CHANNEL: &str = "scenario-bolt";
pub(super) const BOLT_UID: &str = "A1B2C3D4E5F60708";

pub(super) struct DirectFixture {
    pub(super) node_id: NodeId,
    pub(super) inventory: DeviceInventory,
    pub(super) node: ReplayNode,
    pub(super) channel: ReplayChannel,
    pub(super) cassette: HidCassette,
}

pub(super) fn direct_fixture(open_outcome: OpenOutcome, probe_count: usize) -> DirectFixture {
    let node_id = NodeId::from("scenario-direct-node".to_string());
    let product_id = 0xb35b;
    let name = "Scenario Direct Mouse";
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: name.to_string(),
            vendor_id: 0x046d,
            product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: crate::DIRECT_DEVICE_INDEX,
            codename: Some(name.to_string()),
            wpid: None,
            kind: DeviceKind::Unknown,
            online: true,
            battery: None,
            model_info: None,
            capabilities: Some(Capabilities {
                pointer: true,
                ..Capabilities::default()
            }),
        }],
    };
    let mut exchanges = Vec::new();
    for _ in 0..probe_count {
        exchanges.extend(direct_probe_exchanges());
    }
    DirectFixture {
        node_id: node_id.clone(),
        inventory,
        node: ReplayNode {
            info: node_info(node_id, product_id, name),
            presence: NodePresence::Present,
            open_outcome,
            channel: Some(DIRECT_CHANNEL.to_string()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: Vec::new(),
        },
        channel: replay_channel(DIRECT_CHANNEL),
        cassette: cassette("scenario-direct-probes", DIRECT_CHANNEL, exchanges),
    }
}

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

#[derive(Clone, Copy)]
pub(super) struct BoltSlot {
    pub(super) slot: u8,
    pub(super) online: bool,
}

pub(super) struct BoltFixture {
    pub(super) node_id: NodeId,
    pub(super) inventory: DeviceInventory,
    pub(super) node: ReplayNode,
    pub(super) channel: ReplayChannel,
    pub(super) cassette: HidCassette,
}

pub(super) fn bolt_fixture(slots: &[BoltSlot], pass_count: usize) -> BoltFixture {
    let node_id = NodeId::from("scenario-bolt-node".to_string());
    let product_id = 0xc548;
    let mut exchanges = Vec::new();
    for _ in 0..pass_count {
        exchanges.extend(bolt_pass_exchanges(slots));
    }
    BoltFixture {
        node_id: node_id.clone(),
        inventory: DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id,
                unique_id: Some(BOLT_UID.to_string()),
            },
            paired: slots
                .iter()
                .map(|spec| PairedDevice {
                    slot: spec.slot,
                    codename: Some(slot_name(spec.slot)),
                    wpid: None,
                    kind: DeviceKind::Mouse,
                    online: spec.online,
                    battery: None,
                    model_info: None,
                    capabilities: None,
                })
                .collect(),
        },
        node: ReplayNode {
            info: node_info(node_id, product_id, "Scenario Bolt Receiver"),
            presence: NodePresence::Present,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(BOLT_CHANNEL.to_string()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: slots
                .iter()
                .map(|spec| ReceiverSlot {
                    slot: spec.slot,
                    state: ReceiverSlotState::Paired(if spec.online {
                        ReceiverLinkState::Online
                    } else {
                        ReceiverLinkState::Offline
                    }),
                })
                .collect(),
        },
        channel: replay_channel(BOLT_CHANNEL),
        cassette: cassette("scenario-bolt-probes", BOLT_CHANNEL, exchanges),
    }
}

fn bolt_pass_exchanges(slots: &[BoltSlot]) -> Vec<CassetteExchange> {
    let mut exchanges = vec![
        exact(
            vec![0x10, 0xff, 0x81, 0x02, 0, 0, 0],
            Some(vec![
                0x10,
                0xff,
                0x81,
                0x02,
                0,
                u8::try_from(slots.len()).expect("at most six synthetic Bolt slots"),
                0,
            ]),
        ),
        exact(
            vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0],
            Some(unique_id_response()),
        ),
        exact(
            vec![0x10, 0xff, 0x81, 0x00, 0, 0, 0],
            Some(vec![0x10, 0xff, 0x81, 0x00, 0, 1, 0]),
        ),
        exact(
            vec![0x10, 0xff, 0x80, 0x02, 0x02, 0, 0],
            Some(vec![0x10, 0xff, 0x80, 0x02, 0x02, 0, 0]),
        ),
    ];
    for slot in 1..=6 {
        let request = vec![0x10, 0xff, 0x83, 0xb5, 0x50 + slot, 0, 0];
        let response = slots
            .iter()
            .find(|spec| spec.slot == slot)
            .copied()
            .map_or_else(
                || vec![0x10, 0xff, 0x8f, 0x83, 0xb5, 0x02, 0],
                pairing_response,
            );
        exchanges.push(exact(request, Some(response)));
        if slots.iter().any(|spec| spec.slot == slot) {
            exchanges.push(exact(
                vec![0x10, 0xff, 0x83, 0xb5, 0x60 + slot, 0x01, 0],
                Some(codename_response(slot)),
            ));
        }
    }
    for spec in slots.iter().filter(|spec| spec.online) {
        exchanges.push(h20(
            short(spec.slot, 0x00, 0x10, [0, 0, 0]),
            short(spec.slot, 0x00, 0x10, [4, 0, 0]),
        ));
        exchanges.push(h20(
            short(spec.slot, 0x00, 0x00, [0x00, 0x01, 0]),
            short(spec.slot, 0x00, 0x00, [0, 0, 0]),
        ));
    }
    exchanges
}

fn unique_id_response() -> Vec<u8> {
    let mut response = vec![0; 20];
    response[..4].copy_from_slice(&[0x11, 0xff, 0x83, 0xfb]);
    response[4..].copy_from_slice(BOLT_UID.as_bytes());
    response
}

fn pairing_response(spec: BoltSlot) -> Vec<u8> {
    let mut response = vec![0; 20];
    response[..4].copy_from_slice(&[0x11, 0xff, 0x83, 0xb5]);
    response[4] = 0x50 + spec.slot;
    response[5] = 0x02 | u8::from(!spec.online) << 6;
    response[6..8].copy_from_slice(&0xb35b_u16.to_le_bytes());
    response[8..12].copy_from_slice(&[0, 0, 0, spec.slot]);
    response
}

fn codename_response(slot: u8) -> Vec<u8> {
    let name = slot_name(slot);
    let mut response = vec![0; 20];
    response[..4].copy_from_slice(&[0x11, 0xff, 0x83, 0xb5]);
    response[4] = 0x60 + slot;
    response[5] = 1;
    response[6] = u8::try_from(name.len()).expect("short synthetic codename");
    response[7..7 + name.len()].copy_from_slice(name.as_bytes());
    response
}

fn slot_name(slot: u8) -> String {
    format!("slot-{slot}")
}

pub(super) struct DpiFixture {
    pub(super) node_id: NodeId,
    pub(super) route: DeviceRoute,
    pub(super) node: ReplayNode,
    pub(super) channel: ReplayChannel,
    pub(super) cassette: HidCassette,
    pub(super) held_request: Vec<u8>,
}

pub(super) fn malformed_dpi_fixture() -> DpiFixture {
    let bolt = bolt_fixture(
        &[BoltSlot {
            slot: 1,
            online: true,
        }],
        1,
    );
    let held_request = short(1, 0x05, 0x20, [0, 0, 0]);
    let exchanges = vec![
        exact(
            vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0],
            Some(unique_id_response()),
        ),
        h20(
            short(1, 0x00, 0x10, [0, 0, 0]),
            short(1, 0x00, 0x10, [4, 0, 0]),
        ),
        h20(
            short(1, 0x00, 0x00, [0x22, 0x01, 0]),
            short(1, 0x00, 0x00, [0x05, 0, 0]),
        ),
        h20(
            held_request.clone(),
            vec![0x10, 1, 0xff, 0x05, 0x20, 0xff, 0],
        ),
    ];
    DpiFixture {
        node_id: bolt.node_id,
        route: DeviceRoute::Bolt {
            receiver_uid: BOLT_UID.to_string(),
            slot: 1,
        },
        node: bolt.node,
        channel: bolt.channel,
        cassette: cassette("malformed-late-dpi", BOLT_CHANNEL, exchanges),
        held_request,
    }
}

pub(super) fn connection_notification(slot: u8) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..3].copy_from_slice(&[0x11, slot, 0x41]);
    report[4] = 0x02;
    report[5..7].copy_from_slice(&0xb35b_u16.to_le_bytes());
    report
}

fn replay_channel(id: &str) -> ReplayChannel {
    ReplayChannel {
        id: id.to_string(),
        connection: ChannelConnection::Connected,
        report_support: ReportSupport::ShortAndLong,
    }
}

fn node_info(id: NodeId, product_id: u16, name: &str) -> NodeInfo {
    NodeInfo {
        id,
        vendor_id: 0x046d,
        product_id,
        usage_page: 0xff00,
        usage_id: 0x0002,
        name: name.to_string(),
        manufacturer: Some("Logitech".to_string()),
        serial_number: None,
    }
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

fn exact(request: Vec<u8>, response: Option<Vec<u8>>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Exact,
        request,
        response,
        required: true,
    }
}

fn h20(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

pub(super) fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}

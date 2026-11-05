//! Builders shared by the Flow unit tests.

use std::collections::BTreeMap;
use std::sync::Arc;

use openlogi_core::config::{
    FlowConfig, FlowConfigParts, FlowDevice, FlowHostSlot, FlowPeer, FlowPeerName, FlowPublicKey,
};
use openlogi_core::device::DeviceKind;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_flow::generated as proto;
use openlogi_flow::identity::CanonicalDeviceIdentifier;
use openlogi_hid::replay::{ReplayBackend, ReplayTopology};
use openlogi_hid::{ChannelPool, DeviceIoSignal, DeviceRoute, device_io_channel};

use super::devices::InventoryDevice;
use super::runtime::FlowHardware;
use crate::receiver_access::ReceiverAccess;

/// A mouse with unit id `0x0a`.
pub(super) const MOUSE: &str = "unit:0000000a";
/// A keyboard with unit id `0x0b`.
pub(super) const KEYBOARD: &str = "unit:0000000b";
/// A presenter with unit id `0x0c`.
pub(super) const PRESENTER: &str = "unit:0000000c";

pub(super) fn key(value: &str) -> PhysicalDeviceKey {
    PhysicalDeviceKey::parse(value).expect("test keys are canonical")
}

/// The one configured peer.
pub(super) fn peer() -> FlowPeerName {
    FlowPeerName::new("desk").expect("valid peer name")
}

pub(super) fn slot(value: u8) -> FlowHostSlot {
    FlowHostSlot::new(value).expect("valid slot")
}

/// A device on slot 0 here, and on `peer_slot` at [`peer`] when given.
pub(super) fn configured(device: &str, peer_slot: Option<u8>) -> FlowDevice {
    let peers = peer_slot
        .map(|value| BTreeMap::from([(peer(), slot(value))]))
        .unwrap_or_default();
    FlowDevice::new(key(device), slot(0), peers).expect("distinct slots")
}

/// An enabled configuration with [`peer`] and `devices`.
pub(super) fn config(devices: Vec<FlowDevice>) -> FlowConfig {
    config_with_key(FlowPublicKey::from_bytes([7; 32]), devices)
}

/// [`config`] with [`peer`] pinned to `public_key`.
pub(super) fn config_with_key(public_key: FlowPublicKey, devices: Vec<FlowDevice>) -> FlowConfig {
    FlowConfig::try_from(FlowConfigParts {
        enabled: true,
        require_modifier: false,
        peers: vec![FlowPeer {
            name: peer(),
            public_key,
            addresses: Vec::new(),
        }],
        layout: Vec::new(),
        devices,
    })
    .expect("valid Flow config")
}

/// An inventory record on a Bolt receiver, identified by unit id `unit`.
pub(super) fn record(device: &str, kind: DeviceKind, unit: u8, online: bool) -> InventoryDevice {
    InventoryDevice {
        key: key(device),
        route: Some(DeviceRoute::Bolt {
            receiver_uid: "cafe".to_owned(),
            slot: unit,
        }),
        serial: None,
        unit_id: [0, 0, 0, unit],
        kind,
        online,
    }
}

/// The identity a peer names the device with unit id `unit` by.
pub(super) fn unit_identity(unit: u8) -> proto::DeviceIdentity {
    proto::DeviceIdentity {
        ids: vec![CanonicalDeviceIdentifier::unit_id(u32::from(unit)).into()],
        ..Default::default()
    }
}

/// Device access with no device behind it, and the signal that controls its
/// I/O gate. The gate starts open.
pub(super) fn no_hardware() -> (DeviceIoSignal, FlowHardware) {
    let topology = ReplayTopology {
        nodes: Vec::new(),
        channels: Vec::new(),
    };
    let backend = ReplayBackend::new(topology, Vec::new()).expect("an empty topology is valid");
    let (signal, device_io) = device_io_channel();
    let hardware = FlowHardware {
        channel_pool: ChannelPool::with_backend(Arc::new(backend)),
        receiver_access: ReceiverAccess::default(),
        device_io,
    };
    (signal, hardware)
}

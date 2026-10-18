//! The local devices Flow can move, and how they are described to peers.
//!
//! The orchestrator publishes every inventory device that has a physical key,
//! online or not. [`LocalDevices::join`] keeps the ones the Flow configuration
//! names. An offline device stays in the view because its transition to
//! online is the receiver's evidence that a handoff arrived.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Flow runtime later in this stack consumes the view"
    )
)]

use openlogi_core::config::{FlowConfig, FlowDevice, FlowHostSlot, FlowPeerName};
use openlogi_core::device::DeviceKind;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_flow::generated as proto;
use openlogi_flow::identity::{CanonicalDeviceIdentifier, same_device};
use openlogi_hid::DeviceRoute;

/// One inventory device as the orchestrator reports it to Flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InventoryDevice {
    /// The key the Flow configuration names the device by.
    pub(crate) key: PhysicalDeviceKey,
    /// How to address the device, when the inventory can.
    pub(crate) route: Option<DeviceRoute>,
    /// The device serial, when the transport reports one.
    pub(crate) serial: Option<String>,
    /// The HID++ unit id; all zeros when unknown.
    pub(crate) unit_id: [u8; 4],
    /// What the device is.
    pub(crate) kind: DeviceKind,
    /// Whether the device is present and awake on this host.
    pub(crate) online: bool,
}

/// One configured Flow device joined with its inventory record.
#[derive(Clone, Debug)]
pub(crate) struct LocalDevice {
    config: FlowDevice,
    identity: proto::DeviceIdentity,
    route: Option<DeviceRoute>,
    kind: DeviceKind,
    online: bool,
}

impl LocalDevice {
    /// The configuration key.
    pub(crate) const fn key(&self) -> &PhysicalDeviceKey {
        self.config.key()
    }

    /// The identity announced to peers.
    pub(crate) const fn identity(&self) -> &proto::DeviceIdentity {
        &self.identity
    }

    /// Whether the device is present and awake on this host.
    pub(crate) const fn is_online(&self) -> bool {
        self.online
    }

    /// Whether the device moves the cursor, so its arrival places the cursor.
    pub(crate) const fn is_pointing(&self) -> bool {
        is_pointing(self.kind)
    }
}

/// A device this host can send to one peer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutgoingDevice {
    /// How to address the device.
    pub(crate) route: DeviceRoute,
    /// The Easy-Switch slot that reaches the peer.
    pub(crate) slot: FlowHostSlot,
    /// The identity named in the handoff request.
    pub(crate) identity: proto::DeviceIdentity,
}

/// The configured Flow devices present in the inventory, in handoff order:
/// pointing devices first, keyboards last.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocalDevices(Vec<LocalDevice>);

impl LocalDevices {
    /// Join the configured devices with the inventory. A configured device that
    /// the inventory does not list, or that has neither a serial nor a unit id,
    /// cannot take part and is left out.
    pub(crate) fn join(config: &FlowConfig, inventory: &[InventoryDevice]) -> Self {
        let mut devices: Vec<LocalDevice> = config
            .devices()
            .iter()
            .filter_map(|configured| {
                let record = inventory
                    .iter()
                    .find(|record| record.key == *configured.key())?;
                let identity = identity(configured.key(), record)?;
                Some(LocalDevice {
                    config: configured.clone(),
                    identity,
                    route: record.route.clone(),
                    kind: record.kind,
                    online: record.online,
                })
            })
            .collect();
        devices.sort_by(|a, b| {
            handoff_rank(a.kind)
                .cmp(&handoff_rank(b.kind))
                .then_with(|| a.key().cmp(b.key()))
        });
        Self(devices)
    }

    /// Every joined device, in handoff order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &LocalDevice> {
        self.0.iter()
    }

    /// The online devices that can be sent to `peer`, in handoff order.
    pub(crate) fn outgoing(&self, peer: &FlowPeerName) -> Vec<OutgoingDevice> {
        self.0
            .iter()
            .filter(|device| device.online)
            .filter_map(|device| {
                Some(OutgoingDevice {
                    route: device.route.clone()?,
                    slot: device.config.peer_slot(peer.as_str())?,
                    identity: device.identity.clone(),
                })
            })
            .collect()
    }

    /// The local device a peer's identity denotes. A malformed identity
    /// matches nothing.
    pub(crate) fn find(&self, identity: &proto::DeviceIdentity) -> Option<&LocalDevice> {
        self.0
            .iter()
            .find(|device| same_device(identity, &device.identity).unwrap_or(false))
    }

    /// The `AnnounceDevices` whole-state notification at `revision`.
    pub(crate) fn announce(&self, revision: u64) -> proto::AnnounceDevices {
        proto::AnnounceDevices {
            devices: self
                .0
                .iter()
                .map(|device| {
                    let mut view = proto::DeviceView {
                        channel_to_me: u32::from(device.config.self_slot().get()),
                        connected: device.online,
                        // The inventory does not read HID++ 0x1814 host info;
                        // zero tells the peer the count is unknown.
                        host_count: 0,
                        ..Default::default()
                    };
                    *view.identity.get_or_insert_default() = device.identity.clone();
                    view
                })
                .collect(),
            revision,
            ..Default::default()
        }
    }

    /// The identities of the devices this host currently holds.
    pub(crate) fn held(&self) -> Vec<proto::DeviceIdentity> {
        self.0
            .iter()
            .filter(|device| device.online)
            .map(|device| device.identity.clone())
            .collect()
    }
}

/// The identity peers correlate the device by. `None` when the inventory has
/// neither a serial nor a unit id for it.
fn identity(key: &PhysicalDeviceKey, record: &InventoryDevice) -> Option<proto::DeviceIdentity> {
    let mut ids = Vec::with_capacity(2);
    if let Some(serial) = record.serial.as_deref().filter(|serial| !serial.is_empty()) {
        ids.push(CanonicalDeviceIdentifier::serial(serial).into());
    }
    if record.unit_id != [0; 4] {
        ids.push(CanonicalDeviceIdentifier::unit_id(u32::from_be_bytes(record.unit_id)).into());
    }
    (!ids.is_empty()).then(|| proto::DeviceIdentity {
        ids,
        name: key.as_str().to_owned(),
        category: category(record.kind).into(),
        ..Default::default()
    })
}

const fn is_pointing(kind: DeviceKind) -> bool {
    matches!(
        kind,
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
    )
}

/// Pointing devices move first so the cursor follows the user's hand;
/// keyboards move last.
const fn handoff_rank(kind: DeviceKind) -> u8 {
    if is_pointing(kind) {
        0
    } else if kind.is_keyboard_family() {
        2
    } else {
        1
    }
}

const fn category(kind: DeviceKind) -> proto::DeviceCategory {
    if kind.is_keyboard_family() {
        return proto::DeviceCategory::Keyboard;
    }
    match kind {
        DeviceKind::Mouse => proto::DeviceCategory::Mouse,
        DeviceKind::Trackball => proto::DeviceCategory::Trackball,
        DeviceKind::Touchpad => proto::DeviceCategory::Touchpad,
        DeviceKind::Presenter => proto::DeviceCategory::Presenter,
        // The keyboard family returned above.
        DeviceKind::Keyboard
        | DeviceKind::Numpad
        | DeviceKind::Remote
        | DeviceKind::Tablet
        | DeviceKind::Gamepad
        | DeviceKind::Joystick
        | DeviceKind::Headset
        | DeviceKind::Camera
        | DeviceKind::Unknown
        | DeviceKind::Light => proto::DeviceCategory::Other,
    }
}

#[cfg(test)]
mod tests;

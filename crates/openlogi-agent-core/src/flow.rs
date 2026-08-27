//! Agent-side bridge between Flow networking, input edges, and HID++.

mod config;
mod handoff;
mod runtime;

pub use runtime::{FlowController, FlowInputHandle};

use std::collections::BTreeMap;

use openlogi_core::device::DeviceKind;
use openlogi_flow::generated as proto;
use openlogi_flow::identity::CanonicalDeviceIdentifier;
use openlogi_hid::DeviceRoute;

/// Live local facts for one device that can participate in Flow.
#[derive(Clone, Debug)]
pub(crate) struct FlowDeviceSnapshot {
    pub(crate) config_key: String,
    pub(crate) route: Option<DeviceRoute>,
    pub(crate) serial: Option<String>,
    pub(crate) unit_id: [u8; 4],
    pub(crate) kind: DeviceKind,
    pub(crate) online: bool,
}

impl FlowDeviceSnapshot {
    fn identity(&self) -> proto::DeviceIdentity {
        let mut ids = Vec::with_capacity(2);
        if let Some(serial) = self.serial.as_deref().filter(|serial| !serial.is_empty()) {
            ids.push(CanonicalDeviceIdentifier::serial(serial).into());
        }
        if self.unit_id != [0; 4] {
            ids.push(CanonicalDeviceIdentifier::unit_id(u32::from_be_bytes(self.unit_id)).into());
        }
        proto::DeviceIdentity {
            ids,
            name: self.config_key.clone(),
            category: device_category(self.kind).into(),
            ..Default::default()
        }
    }
}

fn device_category(kind: DeviceKind) -> proto::DeviceCategory {
    match kind {
        DeviceKind::Mouse => proto::DeviceCategory::Mouse,
        DeviceKind::Keyboard | DeviceKind::Numpad => proto::DeviceCategory::Keyboard,
        DeviceKind::Trackball => proto::DeviceCategory::Trackball,
        DeviceKind::Touchpad => proto::DeviceCategory::Touchpad,
        DeviceKind::Presenter => proto::DeviceCategory::Presenter,
        DeviceKind::Remote
        | DeviceKind::Tablet
        | DeviceKind::Gamepad
        | DeviceKind::Joystick
        | DeviceKind::Headset
        | DeviceKind::Camera
        | DeviceKind::Unknown
        | DeviceKind::Light => proto::DeviceCategory::Other,
    }
}

fn is_pointing_device(kind: DeviceKind) -> bool {
    matches!(
        kind,
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
    )
}

#[derive(Clone, Debug)]
struct RuntimeDevice {
    snapshot: FlowDeviceSnapshot,
    identity: proto::DeviceIdentity,
    channels: BTreeMap<String, u8>,
}

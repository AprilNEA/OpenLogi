//! AppState unit tests: the shared fixtures, and one module per area.

use std::collections::BTreeMap;
use std::sync::Arc;

use openlogi_camera::Camera;
use openlogi_core::binding::{
    Action, ActionRingIcon, ActionRingSlot, Binding, ButtonId, GestureDirection, RingAction,
};
use openlogi_core::config::{
    Config, DeviceIdentity, LightSettings, Lighting, ScrollResolution, ThumbwheelSensitivity,
    VerticalScrollSensitivity,
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightValueRange, LightValueUnit,
    PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::hid::{
    DeviceRoute, Dpi, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus,
    SmartShiftThreshold, WriteError,
};

use gpui::AppContext as _;
use openlogi_core::app::ForegroundApp;
use openlogi_fixture::{CANONICAL_DEVICE_PROFILE_JSON, DeviceProfile, ProfileSupport};
use openlogi_ipc::{AgentSnapshot, AgentStatus, ForegroundApps, InventoryHealth, PROTOCOL_VERSION};

use crate::features::mouse::thumbwheel::ThumbwheelPreset;
use crate::services::assets::AssetResolver;
use crate::services::ipc::SetLight;
#[cfg(target_os = "macos")]
use crate::services::ipc::SetLightManualPower;

use super::bindings::apply_thumbwheel_pair;
use super::devices::build_device_list;
use super::scroll::set_scroll_resolution_if_supported;
use super::smartshift::{
    ConfirmationOutcome, SmartShiftDeviceState, smartshift_read_is_current,
    smartshift_write_outcome,
};
use super::{
    AppState, ConfigPersistence, DeviceKey, DeviceRecord, INVENTORY_MISS_GRACE, LightCommandStatus,
    Load, Sources, StateEvent,
};

mod asset_targets;
mod bindings;
mod camera;
mod device_list;
mod device_names;
mod lighting;
mod profile_scope;
mod reload;
mod smartshift;
mod transient_identity;
mod wheel_resolution;

/// Config key of the mouse [`direct_inventory`] builds with a real unit id.
///
/// The transport-free identity, not the `direct:046d:b023:…` route it is
/// reached on: a device whose unit id is known resolves to its identity key,
/// which is what settings are now written under.
pub(super) const KNOWN_MOUSE_KEY: &str = "unit:a393cae0";

fn direct_inventory(unit_id: [u8; 4]) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".to_string(),
            vendor_id: 0x046d,
            product_id: 0xb023,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

fn superseded_litra_light() -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-superseded".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-superseded".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    }
}

/// What a light-write result that belonged to a live request announces.
fn lighting_changed(key: &DeviceKey) -> StateEvent {
    StateEvent::LightingChanged(key.clone())
}

/// A state holding the one persistent mouse, so per-device config has a key.
pub(super) fn state_with_a_known_mouse() -> AppState {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .unwrap()
        .dpi_gestures = true;
    AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    })
}

fn app(id: &str, display_name: &str) -> ForegroundApp {
    ForegroundApp {
        id: id.to_string(),
        display_name: display_name.to_string(),
    }
}

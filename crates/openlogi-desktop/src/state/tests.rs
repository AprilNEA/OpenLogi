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
#[cfg(target_os = "macos")]
use openlogi_ipc::{PrimaryMouseButton, SystemMouseSettingError};

use crate::features::mouse::thumbwheel::ThumbwheelPreset;
use crate::services::assets::AssetResolver;
use crate::services::ipc::SetLight;
#[cfg(target_os = "macos")]
use crate::services::ipc::SetLightManualPower;
#[cfg(target_os = "macos")]
use crate::services::ipc::{Command, PrimaryMouseButtonCommandError, SetPrimaryMouseButton};

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

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_failure_stays_visible_without_overwriting_the_snapshot() {
    let cache = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &cache, commands));
    let _ = state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert_eq!(
        state.request_primary_mouse_button(PrimaryMouseButton::Right),
        [StateEvent::SettingsChanged]
    );
    assert!(state.primary_mouse_button_pending());
    assert!(state.primary_mouse_button_error().is_none());
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::SetPrimaryMouseButton(SetPrimaryMouseButton {
            button: PrimaryMouseButton::Right
        }))
    ));

    let error = PrimaryMouseButtonCommandError::Rejected(SystemMouseSettingError::Unavailable {
        message: "persistent write rejected".into(),
    });
    assert_eq!(
        state.apply_primary_mouse_button_result(Err(error.clone())),
        [StateEvent::SettingsChanged]
    );
    assert!(!state.primary_mouse_button_pending());
    assert_eq!(state.primary_mouse_button_error(), Some(error.clone()));
    assert_eq!(
        state.primary_mouse_button(),
        Some(PrimaryMouseButton::Left),
        "a command result must not replace the agent's authoritative snapshot"
    );
    assert_eq!(
        state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.primary_mouse_button_error(),
        Some(error),
        "a matching snapshot must not hide a definitive platform rejection"
    );
    assert_eq!(
        state.set_primary_mouse_button(Some(PrimaryMouseButton::Left)),
        [StateEvent::SettingsChanged]
    );

    assert_eq!(
        state.request_primary_mouse_button(PrimaryMouseButton::Right),
        [StateEvent::SettingsChanged]
    );
    assert!(state.primary_mouse_button_error().is_none());
    assert_eq!(
        state.apply_primary_mouse_button_result(Ok(PrimaryMouseButton::Right)),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.primary_mouse_button(),
        Some(PrimaryMouseButton::Left),
        "even success waits for the observed snapshot to move the switch"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_snapshot_reconciles_an_ambiguous_transport_failure() {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &cache, commands));
    let _ = state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert_eq!(
        state.request_primary_mouse_button(PrimaryMouseButton::Right),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.apply_primary_mouse_button_result(Err(
            PrimaryMouseButtonCommandError::AgentUnavailable,
        )),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.primary_mouse_button_error(),
        Some(PrimaryMouseButtonCommandError::AgentUnavailable)
    );
    assert_eq!(
        state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)),
        [StateEvent::SettingsChanged]
    );
    assert!(state.primary_mouse_button_error().is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_snapshot_before_lost_reply_also_confirms_the_write() {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &cache, commands));
    let _ = state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert_eq!(
        state.request_primary_mouse_button(PrimaryMouseButton::Right),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(
        state.apply_primary_mouse_button_result(Err(
            PrimaryMouseButtonCommandError::AgentUnavailable,
        )),
        [StateEvent::SettingsChanged]
    );
    assert!(state.primary_mouse_button_error().is_none());
}

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

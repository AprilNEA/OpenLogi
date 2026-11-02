//! Orchestrator tests: the shared fixtures, and one module per area.

use super::{
    AgentDevice, FollowerCookieState, FollowerCookieWrite, InventoryHealth, Orchestrator,
    RECONNECT_REAPPLY_CONFIRM_RETRIES, VOLATILE_REAPPLY_CONFIRM_RETRIES, build_devices,
    changed_follower_cookies, changed_force_sensing, configured_disabled_keys,
    configured_force_sensing, configured_report_rate, configured_wheel_mode, desktop_switch,
    host_switch_links, keeps_os_layout, main_wheel_divert, newly_kept_os_layouts,
    os_layout_appeared, pick_current, plan_reapply, reapply_targets, reconnect_follower_cookies,
    settings_moved, stable_id, wheel_inverted,
};
use crate::hardware::WheelModeChange;
use crate::watchers::keyboard::KeyboardSpec;
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{Action, Binding, ButtonId, GridKey};
use openlogi_core::config::{
    Config, DeviceConfig, DisableKey, LightSettings, LinkConfig, ScrollResolution,
    VerticalScrollSensitivity,
};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
    LightCapabilities, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::{Dpi, ReportRate};
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::observable::ObservableState;

mod camera;
mod capture_plans;
mod device_list;
mod device_settings;
mod disable_keys_native_write;
mod disabled_keys;
mod flow;
mod light_changes;
mod light_reload;
mod lock_keys;
mod low_battery;
mod os_layout;
mod pointer_speed;
mod publication;
mod reapply;

/// An orchestrator wired to a state cell nobody subscribes to. The publishing
/// paths still run, so a mutator that stops republishing shows up here rather
/// than only in the running agent.
fn orchestrator(config: Config) -> Orchestrator {
    Orchestrator::new(config, Arc::new(ObservableState::new("test".to_string())))
}

fn dev(key: &str, slot: u8, online: bool) -> AgentDevice {
    AgentDevice {
        config_key: key.to_string(),
        model_key: key.to_string(),
        route: Some(DeviceRoute::Bolt {
            receiver_uid: "AA00".to_string(),
            slot,
        }),
        slot,
        serial: None,
        unit_id: [0; 4],
        capabilities: None,
        kind: openlogi_core::device::DeviceKind::Mouse,
        light_capabilities: None,
        online,
        settings_revision: 0,
        codename: None,
        battery: None,
    }
}

/// The one keyboard spec `orch` publishes, or `None` when it publishes none.
fn sole_keyboard_spec(orch: &Orchestrator) -> Option<KeyboardSpec> {
    let mut specs = orch.keyboard_specs_for();
    assert!(
        specs.len() <= 1,
        "expected at most one keyboard spec: {specs:?}"
    );
    specs.pop()
}

fn direct_inventory(serial_number: Option<&str>, unit_id: [u8; 4]) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".to_string(),
            vendor_id: 0x046d,
            product_id: 0xb023,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: serial_number.map(str::to_string),
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            settings_revision: 0,
            hidpp10: false,
        }],
    }
}

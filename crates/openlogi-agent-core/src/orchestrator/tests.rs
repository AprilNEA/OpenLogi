//! Orchestrator tests: the shared fixtures, and one module per area.

use super::{
    AgentDevice, InventoryHealth, Orchestrator, VOLATILE_REAPPLY_CONFIRM_RETRIES,
    any_device_needs_capture_rearm, build_devices, configured_wheel_mode, host_switch_links,
    pick_current, plan_reapply, reapply_targets, stable_id,
};
use crate::hardware::WheelModeChange;
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{Action, Binding, ButtonId, LongPressBinding};
use openlogi_core::config::{
    Config, DeviceConfig, LightSettings, LinkConfig, ScrollResolution, VerticalScrollSensitivity,
};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
    LightCapabilities, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::Dpi;
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
use std::sync::Arc;

use crate::observable::ObservableState;

mod camera;
mod capture_plans;
mod device_list;
mod device_settings;
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
    }
}

/// A keyboard-kind device. `keyboard_spec_for` selects on [`DeviceKind`], so
/// the mouse-shaped [`dev`] helper cannot stand in for one.
fn keyboard_dev(key: &str, slot: u8) -> AgentDevice {
    AgentDevice {
        kind: DeviceKind::Keyboard,
        ..dev(key, slot, true)
    }
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
        }],
    }
}

#[test]
fn a_bound_keyboard_key_is_diverted_and_an_untouched_one_stays_native() {
    // Diversion is what costs a key its firmware behavior: `0x00e2`/`0x00e3`
    // are the backlight-adjust pair, so a key that reaches `wanted` without
    // the user binding it silently takes away backlight control. The gate is
    // the `wanted` filter, and nothing else covers it.
    use std::collections::BTreeMap;

    let mut config = Config::default();
    let mut orch = orchestrator(config.clone());
    orch.devices = vec![keyboard_dev("kb", 1)];
    assert!(
        orch.keyboard_spec_for().is_none(),
        "a keyboard with no bound keys must not open a capture session at all"
    );

    config.set_binding(
        "kb",
        ButtonId::KeyBacklightDown,
        Binding::Single(Action::BrightnessDown),
    );
    let mut orch = orchestrator(config.clone());
    orch.devices = vec![keyboard_dev("kb", 1)];
    let spec = orch
        .keyboard_spec_for()
        .expect("one bound key opens the session");
    assert_eq!(
        spec.wanted,
        BTreeMap::from([(0x00e2, ButtonId::KeyBacklightDown)]),
        "only the bound key is diverted — Backlight Up keeps its firmware behavior"
    );

    // `Action::None` is a binding present in the config but not a reason to
    // take the key away from the firmware.
    config.set_binding(
        "kb",
        ButtonId::KeyBacklightUp,
        Binding::Single(Action::None),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard_dev("kb", 1)];
    let spec = orch
        .keyboard_spec_for()
        .expect("the other bound key still opens the session");
    assert!(
        !spec
            .wanted
            .values()
            .any(|&button| button == ButtonId::KeyBacklightUp),
        "a key bound to Action::None must stay native"
    );

    // A long-press binding is diverted on the strength of its threshold
    // action alone: the short action is `None` here, and diverting anyway is
    // what lets the long press fire at all.
    let mut config = Config::default();
    config.set_binding(
        "kb",
        ButtonId::KeyBacklightUp,
        Binding::LongPress(LongPressBinding::new(Action::None, Action::MissionControl)),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard_dev("kb", 1)];
    let spec = orch
        .keyboard_spec_for()
        .expect("a long-press binding opens the session");
    assert_eq!(
        spec.wanted,
        BTreeMap::from([(0x00e3, ButtonId::KeyBacklightUp)]),
        "a long-press binding diverts its own key and no other"
    );
}

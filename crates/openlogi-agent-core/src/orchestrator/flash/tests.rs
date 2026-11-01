//! Which mice are flash-bindings mice, when the orchestrator asks their
//! flash for a change, and the bindings it hands the hook.

use std::sync::Arc;

use openlogi_core::binding::{Action, Binding, ButtonId, FlashWrite, G700, SlotIntent};
use openlogi_core::config::{Config, DeviceConfig};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};

use super::super::Orchestrator;
use super::flash_devices;
use crate::observable::ObservableState;

fn g700_inventory(online: bool) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "G700 receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc531,
            unique_id: Some("C531A1".into()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: None,
            wpid: Some(0x1023),
            kind: DeviceKind::Mouse,
            online,
            battery: None,
            model_info: None,
            capabilities: Some(Capabilities {
                pointer: true,
                report_rate: true,
                flash_bindings: true,
                ..Capabilities::default()
            }),
            settings_revision: 0,
            hidpp10: true,
        }],
    }
}

fn g700_key() -> String {
    flash_devices(&Config::default(), &[g700_inventory(true)])
        .pop()
        .expect("the G700 is a flash-bindings mouse")
        .config_key
}

fn config_binding(button: ButtonId, action: Action) -> Config {
    let mut config = Config::default();
    let mut device = DeviceConfig::default();
    device.bindings.insert(button, Binding::Single(action));
    config.devices.insert(g700_key(), device);
    config
}

fn orchestrator(config: Config) -> Orchestrator {
    Orchestrator::new(config, Arc::new(ObservableState::new("test".to_string())))
}

fn route(orch: &Orchestrator) -> openlogi_hid::DeviceRoute {
    orch.flash_devices[0].route.clone()
}

#[test]
fn only_a_mouse_that_reports_flash_bindings_is_one() {
    let devices = flash_devices(&Config::default(), &[g700_inventory(false)]);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].layout.slots, G700.slots);
    assert!(!devices[0].online);

    let mut other = g700_inventory(true);
    other.paired[0].capabilities = Some(Capabilities::presumed_from_kind(DeviceKind::Mouse));
    assert!(flash_devices(&Config::default(), &[other]).is_empty());
}

#[test]
fn an_online_g700_is_asked_for_its_configured_bindings_once() {
    let mut orch = orchestrator(config_binding(ButtonId::G6, Action::Copy));
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    let route = route(&orch);
    assert_eq!(
        orch.shared.flash_writes.state(&route).write,
        FlashWrite::Pending
    );
    let asked = orch.flash_requested.get(&route).cloned().expect("asked");
    assert_eq!(
        asked[G700.slot_of(ButtonId::G6).map(usize::from).expect("slot")],
        SlotIntent::Agent
    );
    assert!(
        asked
            .iter()
            .enumerate()
            .all(|(slot, intent)| slot == 5 || *intent == SlotIntent::Keep),
        "{asked:?}"
    );
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    assert_eq!(orch.flash_requested.get(&route), Some(&asked));

    let maps = orch.shared.hook_maps.read().expect("maps");
    assert_eq!(
        maps.flash_slot_bindings.get(&ButtonId::G6),
        Some(&Binding::Single(Action::Copy))
    );
    assert!(
        maps.flash_slot_bindings
            .keys()
            .all(|button| G700.slots.iter().any(|slot| slot.button == *button)),
        "only the G700's own slots"
    );
}

#[test]
fn a_mouse_that_sleeps_is_synced_again_when_it_returns() {
    let mut orch = orchestrator(config_binding(ButtonId::G6, Action::Copy));
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    let route = route(&orch);
    orch.refresh_inventory(&[g700_inventory(false)], &[], false);
    assert!(orch.flash_requested.is_empty());
    assert!(
        orch.shared
            .hook_maps
            .read()
            .expect("maps")
            .flash_slot_bindings
            .is_empty(),
        "an offline mouse runs nothing"
    );
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    assert!(orch.flash_requested.contains_key(&route));
}

#[test]
fn a_config_change_asks_the_flash_again() {
    let mut orch = orchestrator(config_binding(ButtonId::G6, Action::Copy));
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    let route = route(&orch);
    orch.reload_config(config_binding(ButtonId::G6, Action::None));
    let slot = usize::from(G700.slot_of(ButtonId::G6).expect("slot"));
    assert!(matches!(
        orch.flash_requested[&route][slot],
        SlotIntent::Native(_)
    ));
}

#[test]
fn a_disabled_g700_is_left_alone() {
    let mut config = config_binding(ButtonId::G6, Action::Copy);
    if let Some(device) = config.devices.get_mut(&g700_key()) {
        device.enabled = false;
    }
    let mut orch = orchestrator(config);
    orch.refresh_inventory(&[g700_inventory(true)], &[], false);
    assert!(orch.flash_requested.is_empty());
    assert_eq!(
        orch.shared.flash_writes.state(&route(&orch)).write,
        FlashWrite::Idle
    );
    assert!(
        orch.shared
            .hook_maps
            .read()
            .expect("maps")
            .flash_slot_bindings
            .is_empty()
    );
}

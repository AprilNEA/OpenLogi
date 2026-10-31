//! Which devices' clicks the OS hook filters for switch bounce (#2182).

use openlogi_core::config::ClickDebounce;

use super::*;
use crate::runtime::hook::{DebouncedSources, InputSource};

/// A G903 on a Lightspeed receiver, as the inventory reports it.
fn g903_inventory(online: bool) -> DeviceInventory {
    let mut inventory = direct_inventory(None, [9, 0, 3, 1]);
    inventory.receiver.name = "USB Receiver".to_string();
    inventory.receiver.product_id = 0xc539;
    inventory.receiver.unique_id = Some("LS01".into());
    inventory.paired[0].slot = 1;
    inventory.paired[0].codename = Some("G903".to_string());
    inventory.paired[0].wpid = Some(0x4087);
    inventory.paired[0].online = online;
    inventory
}

fn window(ms: u8) -> ClickDebounce {
    ClickDebounce::try_new(ms).expect("valid test window")
}

fn published(orch: &Orchestrator) -> DebouncedSources {
    orch.shared
        .hook_maps
        .read()
        .expect("hook maps")
        .click_debounce
        .clone()
}

fn g903_sources(ms: u8) -> DebouncedSources {
    let mut sources = DebouncedSources::default();
    for source in InputSource::of_pairing(&g903_inventory(true).receiver, Some(0x4087)) {
        sources.insert(source, window(ms));
    }
    sources
}

/// An orchestrator that sees the G903 online, with `ms` configured for it.
fn orchestrator_with_debounce(ms: Option<u8>) -> Orchestrator {
    let mut orch = orchestrator(Config::default());
    orch.refresh_inventory(&[g903_inventory(true)], &[], false);
    let key = orch.devices[0].config_key.clone();
    let mut config = Config::default();
    config.set_click_debounce(&key, ms.map(window));
    orch.reload_config(config);
    orch
}

#[test]
fn a_configured_window_filters_the_receiver_and_the_paired_device_ids() {
    let orch = orchestrator_with_debounce(Some(30));
    let expected = if openlogi_hook::BUTTON_SOURCES_ATTRIBUTED {
        g903_sources(30)
    } else {
        DebouncedSources::default()
    };
    assert_eq!(published(&orch), expected);
}

#[test]
fn no_window_filters_nothing() {
    let orch = orchestrator_with_debounce(None);
    assert_eq!(published(&orch), DebouncedSources::default());
}

#[test]
fn a_device_that_sleeps_loses_its_filter_and_gets_it_back_on_wake() {
    if !openlogi_hook::BUTTON_SOURCES_ATTRIBUTED {
        return;
    }
    let mut orch = orchestrator_with_debounce(Some(30));
    // Only `online` changes, so this takes the inventory fast path.
    orch.refresh_inventory(&[g903_inventory(false)], &[], false);
    assert_eq!(published(&orch), DebouncedSources::default());
    orch.refresh_inventory(&[g903_inventory(true)], &[], false);
    assert_eq!(published(&orch), g903_sources(30));
}

#[test]
fn a_disabled_device_is_not_filtered() {
    let mut orch = orchestrator_with_debounce(Some(30));
    let key = orch.devices[0].config_key.clone();
    let mut config = orch.config.clone();
    config.devices.entry(key).or_default().enabled = false;
    orch.reload_config(config);
    assert_eq!(published(&orch), DebouncedSources::default());
}

#[test]
fn a_rebuild_for_another_reason_keeps_the_filter() {
    if !openlogi_hook::BUTTON_SOURCES_ATTRIBUTED {
        return;
    }
    let mut orch = orchestrator_with_debounce(Some(45));
    orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
    assert_eq!(published(&orch), g903_sources(45));
}

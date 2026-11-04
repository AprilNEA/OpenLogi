//! What the orchestrator hands to Flow.

use super::*;

#[test]
fn a_reload_hands_flow_the_new_configuration() {
    let mut orch = orchestrator(Config::default());
    assert_eq!(*orch.shared.flow.current_config(), Config::default().flow);

    let mut config = Config::default();
    config.flow.require_modifier = true;
    orch.reload_config(config.clone());
    assert_eq!(*orch.shared.flow.current_config(), config.flow);
}

#[test]
fn the_inventory_reaches_flow_keyed_like_capture() {
    let mut orch = orchestrator(Config::default());
    orch.refresh_inventory(
        &[direct_inventory(Some("serial-1"), [1, 2, 3, 4])],
        &[],
        false,
    );

    let devices = orch.shared.flow.current_devices();
    assert_eq!(devices.len(), 1);
    let (key, route) =
        Orchestrator::capture_identity(&orch.devices[0]).expect("online with a route");
    assert_eq!(devices[0].key, key);
    assert_eq!(devices[0].route, Some(route));
    assert_eq!(devices[0].serial.as_deref(), Some("serial-1"));
    assert!(devices[0].online);
}

/// Flow learns which devices can switch computers from the measured feature
/// table: not a HID++ 2.0 device without `0x1814`, nor a HID++ 1.0 one.
#[test]
fn flow_learns_which_devices_can_switch_hosts() {
    let switches = |capabilities: Option<Capabilities>, hidpp10: bool| {
        let mut inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);
        inventory.paired[0].capabilities = capabilities;
        inventory.paired[0].hidpp10 = hidpp10;
        let mut orch = orchestrator(Config::default());
        orch.refresh_inventory(&[inventory], &[], false);
        orch.shared.flow.current_devices()[0].switches_hosts
    };

    assert!(switches(
        Some(Capabilities::from_feature_ids(&[0x1814])),
        false
    ));
    assert!(!switches(
        Some(Capabilities::from_feature_ids(&[0x2201])),
        false
    ));
    assert!(!switches(None, true));
    assert!(switches(None, false), "not probed yet");
}

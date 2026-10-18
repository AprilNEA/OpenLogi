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

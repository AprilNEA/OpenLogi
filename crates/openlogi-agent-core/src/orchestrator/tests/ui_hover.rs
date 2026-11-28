//! Which device, if any, UI hover haptics tick on.

use super::*;

fn with_haptics(device: AgentDevice) -> AgentDevice {
    AgentDevice {
        capabilities: Some(Capabilities {
            haptic_feedback: true,
            ..Capabilities::default()
        }),
        ..device
    }
}

fn hover_on() -> Config {
    let mut config = Config::default();
    config.app_settings.ui_hover_haptics = true;
    config
}

#[test]
fn hover_route_follows_selected_link() {
    let direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb042,
    };
    let mut orch = orchestrator(hover_on());
    orch.devices = vec![
        with_haptics(dev("mx", 1, false)),
        with_haptics(AgentDevice {
            route: Some(direct.clone()),
            ..dev("mx", 0, true)
        }),
    ];
    orch.reload_config(hover_on());
    assert_eq!(
        orch.ui_hover_haptic_route(),
        Some(direct),
        "the offline Bolt entry under the same key must not silence the online link"
    );
}

#[test]
fn disabled_device_has_no_hover_route() {
    let mut config = hover_on();
    config.set_device_enabled("mx", false);
    let mut orch = orchestrator(config.clone());
    orch.devices = vec![with_haptics(dev("mx", 1, true))];
    orch.reload_config(config);
    assert_eq!(orch.ui_hover_haptic_route(), None);
}

#[test]
fn hover_route_needs_the_preference_and_haptics() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![with_haptics(dev("mx", 1, true))];
    orch.reload_config(Config::default());
    assert_eq!(orch.ui_hover_haptic_route(), None, "preference off");

    orch.devices = vec![dev("mx", 1, true)];
    orch.reload_config(hover_on());
    assert_eq!(orch.ui_hover_haptic_route(), None, "no HID++ haptics");

    orch.devices = vec![with_haptics(dev("mx", 1, true))];
    orch.reload_config(hover_on());
    assert!(orch.ui_hover_haptic_route().is_some());
}

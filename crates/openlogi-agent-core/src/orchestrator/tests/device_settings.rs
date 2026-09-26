//! Per-device settings the orchestrator derives: wheel mode, host-switch links and the DPI cycle.

use super::*;

#[test]
fn configured_wheel_mode_gates_resolution_and_inversion_independently() {
    let mut config = Config::default();
    config.set_scroll_resolution("a", Some(ScrollResolution::Low));
    config.set_invert_scroll("a", true);
    let mut device = dev("a", 1, true);

    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        scroll_inversion: false,
        ..Capabilities::default()
    });
    assert_eq!(
        configured_wheel_mode(&config, &device),
        Some(WheelModeChange::Resolution(ScrollResolution::Low))
    );

    device.capabilities = Some(Capabilities {
        hires_wheel: false,
        scroll_inversion: true,
        ..Capabilities::default()
    });
    assert_eq!(
        configured_wheel_mode(&config, &device),
        Some(WheelModeChange::Inversion(true))
    );

    device.capabilities = None;
    assert_eq!(configured_wheel_mode(&config, &device), None);
}

#[test]
fn configured_wheel_mode_leaves_unset_resolution_unmanaged() {
    let config = Config::default();
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        scroll_inversion: false,
        ..Capabilities::default()
    });

    assert_eq!(configured_wheel_mode(&config, &device), None);
}

#[test]
fn host_switch_links_keep_sleeping_targets_but_require_online_keyboard() {
    let mut config = Config::default();
    config
        .devices
        .entry("keyboard".into())
        .or_default()
        .host_switch_targets = vec!["mouse".into(), "offline".into(), "missing".into()];
    let devices = [
        dev("keyboard", 1, true),
        dev("mouse", 2, true),
        dev("offline", 3, false),
    ];

    let links = host_switch_links(&config, &devices);

    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].keyboard,
        DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        }
    );
    assert_eq!(
        links[0].targets,
        vec![
            DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 2,
            },
            DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 3,
            }
        ]
    );
}

#[test]
fn dpi_cycle_drops_offline_device_and_restores_on_return() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![dev("mouse", 1, true)];
    orch.rebuild();
    {
        let Ok(mut dpi) = orch.shared.dpi_cycle.write() else {
            panic!("DPI cycle lock should not be poisoned");
        };
        if let Some(state) = dpi.by_key.get_mut("mouse") {
            state.index = 3;
        }
    }

    orch.devices[0].online = false;
    orch.publish_device_runtime();
    {
        let Ok(dpi) = orch.shared.dpi_cycle.read() else {
            panic!("DPI cycle lock should not be poisoned");
        };
        assert!(!dpi.by_key.contains_key("mouse"));
    }

    orch.devices[0].online = true;
    orch.publish_device_runtime();
    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    assert_eq!(dpi.by_key.get("mouse").map(|s| s.index), Some(0));
    assert_eq!(
        dpi.by_key.get("mouse").and_then(|s| s.target.clone()),
        orch.devices[0].route
    );
}

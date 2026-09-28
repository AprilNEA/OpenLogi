//! Per-device settings the orchestrator derives: wheel mode, host-switch links and the DPI cycle.

use std::collections::BTreeMap;

use openlogi_hid::reprog_controls::BACK_CIDS;

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

fn keyboard(key: &str, slot: u8, online: bool) -> AgentDevice {
    let mut device = dev(key, slot, online);
    device.kind = DeviceKind::Keyboard;
    device
}

#[test]
fn keyboard_spec_diverts_exactly_the_bound_controls() {
    let mut config = Config::default();
    // A catalogued key, a key OpenLogi has no catalog row for, and a key
    // bound to nothing — only the first two are diverted, by their CID.
    config.set_binding(
        "kbd",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    config.set_binding(
        "kbd",
        ButtonId::control(0x01f3),
        Binding::Single(Action::Copy),
    );
    config.set_binding(
        "kbd",
        ButtonId::control(0x00e5),
        Binding::Single(Action::None),
    );
    // A K380's multiplatform Back key is a member of the mouse Back family,
    // but bound by number it is this keyboard's key and is diverted here.
    let [_, multiplatform_back, _, _] = BACK_CIDS;
    config.set_binding(
        "kbd",
        ButtonId::control(multiplatform_back),
        Binding::Single(Action::Paste),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];

    let spec = orch.keyboard_spec_for().expect("bound keys publish a spec");
    assert_eq!(
        spec.wanted,
        BTreeMap::from([
            (multiplatform_back, ButtonId::control(multiplatform_back)),
            (0x010a, ButtonId::control(0x010a)),
            (0x01f3, ButtonId::control(0x01f3)),
        ])
    );
    assert_eq!(spec.config_key, "kbd");
}

#[test]
fn keyboard_spec_is_absent_without_a_real_binding_or_for_a_disabled_keyboard() {
    let mut config = Config::default();
    config.set_binding(
        "kbd",
        ButtonId::control(0x010a),
        Binding::Single(Action::None),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert!(orch.keyboard_spec_for().is_none(), "None is not a binding");

    let mut config = Config::default();
    config.set_binding(
        "kbd",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    config.set_device_enabled("kbd", false);
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert!(
        orch.keyboard_spec_for().is_none(),
        "a disabled keyboard is left fully native"
    );
}

#[test]
fn keyboard_spec_prefers_the_online_keyboard_over_a_sleeping_slot() {
    // The same MX Keys Mini paired three times (#1581): the stale slots sort
    // first but must not take the session from the keyboard that is typing.
    let mut config = Config::default();
    for key in ["stale", "live"] {
        config.set_binding(
            key,
            ButtonId::control(0x010a),
            Binding::Single(Action::Screenshot),
        );
    }
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("stale", 1, false), keyboard("live", 2, true)];
    assert_eq!(orch.keyboard_spec_for().expect("spec").config_key, "live");

    // With every bound keyboard asleep, inventory order breaks the tie.
    orch.devices = vec![keyboard("stale", 1, false), keyboard("live", 2, false)];
    assert_eq!(orch.keyboard_spec_for().expect("spec").config_key, "stale");
}

#[test]
fn keyboard_spec_skips_a_keyboard_with_no_bound_controls() {
    // Two keyboards, bindings only on the one inventory order puts second:
    // the session goes to the bound keyboard, not to the first one found.
    let mut config = Config::default();
    config.set_binding(
        "bound",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("unbound", 1, true), keyboard("bound", 2, true)];
    assert_eq!(orch.keyboard_spec_for().expect("spec").config_key, "bound");

    // An unbound keyboard being online does not make it a candidate: the
    // bound one holds the session even while asleep.
    orch.devices = vec![keyboard("unbound", 1, true), keyboard("bound", 2, false)];
    assert_eq!(orch.keyboard_spec_for().expect("spec").config_key, "bound");
}

#[test]
fn keyboard_spec_ignores_an_unbound_stale_slot_while_the_live_keyboard_sleeps() {
    // #1581: the stale receiver slots of a re-paired MX Keys Mini carry no
    // bindings. When the live slot naps, the session must stay on it rather
    // than be torn down because an unbound slot sorted first.
    let mut config = Config::default();
    config.set_binding(
        "live",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("stale", 1, false), keyboard("live", 2, false)];
    assert_eq!(orch.keyboard_spec_for().expect("spec").config_key, "live");
}

#[test]
fn keyboard_spec_never_diverts_reserved_controls() {
    // The Easy-Switch host keys belong to the host-switch session and the
    // primary clicks must never be swallowed; a binding that names one is
    // left native, and a binding naming only those publishes no session.
    let mut config = Config::default();
    config.set_binding(
        "kbd",
        ButtonId::control(0x00d1),
        Binding::Single(Action::Copy),
    );
    config.set_binding(
        "kbd",
        ButtonId::control(0x0050),
        Binding::Single(Action::Copy),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert!(orch.keyboard_spec_for().is_none());

    let mut config = Config::default();
    config.set_binding(
        "kbd",
        ButtonId::control(0x00d1),
        Binding::Single(Action::Copy),
    );
    config.set_binding(
        "kbd",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert_eq!(
        orch.keyboard_spec_for().expect("spec").wanted,
        BTreeMap::from([(0x010a, ButtonId::control(0x010a))])
    );
}

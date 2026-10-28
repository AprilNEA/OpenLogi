//! Per-device settings the orchestrator derives: wheel mode, host-switch links and the DPI cycle.

use std::collections::BTreeMap;

use openlogi_hid::reprog_controls::BACK_CIDS;
use openlogi_hid::session::gesture::MainWheelDivert;

use super::*;

/// A wheel-hold button scrolls sideways the way its main wheel scrolls: with
/// the native invert bit, as the link it is reached by is set; without it, as
/// the hook reverses it from the device-level setting.
#[test]
fn held_wheel_inversion_follows_the_main_wheel_on_its_link() {
    let mut config = Config::default();
    config.set_invert_scroll("a", true);
    let mut device = dev("a", 1, true);
    assert!(
        !wheel_inverted(&config, &device),
        "unknown capabilities invert nothing"
    );
    device.capabilities = Some(Capabilities {
        scroll_inversion: true,
        ..Capabilities::default()
    });
    assert!(wheel_inverted(&config, &device));
    let mut link = LinkConfig::default();
    link.overrides.invert_scroll = Some(false);
    config
        .devices
        .get_mut("a")
        .expect("device entry")
        .links
        .insert(stable_id(&device).route_key(), link);
    assert!(
        !wheel_inverted(&config, &device),
        "the link's override is what the firmware is written"
    );
    device.capabilities = Some(Capabilities {
        scroll_inversion: false,
        ..Capabilities::default()
    });
    assert!(
        wheel_inverted(&config, &device),
        "the hook reverses from the device-level setting"
    );
}

#[test]
fn disable_keys_reapply_distinguishes_unmanaged_empty_and_nonempty_config() {
    let mut config = Config::default();
    assert_eq!(configured_disabled_keys(&config, "keyboard"), None);

    config.set_disabled_keys("keyboard", BTreeSet::new());
    assert_eq!(
        configured_disabled_keys(&config, "keyboard"),
        Some(openlogi_hid::DisableKeysMask::EMPTY)
    );

    config.set_disabled_keys(
        "keyboard",
        BTreeSet::from([DisableKey::CapsLock, DisableKey::WindowsCommand]),
    );
    assert_eq!(
        configured_disabled_keys(&config, "keyboard"),
        Some(
            openlogi_hid::DisableKeysMask::CAPS_LOCK
                | openlogi_hid::DisableKeysMask::WINDOWS_COMMAND
        )
    );
}

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
        configured_wheel_mode(&config, &device, true),
        Some(WheelModeChange::Resolution(ScrollResolution::Low))
    );

    device.capabilities = Some(Capabilities {
        hires_wheel: false,
        scroll_inversion: true,
        ..Capabilities::default()
    });
    assert_eq!(
        configured_wheel_mode(&config, &device, true),
        Some(WheelModeChange::Inversion(true))
    );

    device.capabilities = None;
    assert_eq!(configured_wheel_mode(&config, &device, true), None);
}

/// Smooth scrolling diverts the wheel only with the device's opt-in, its
/// `0x2121` wheel, the mouse hook and mouse capture. The divert carries the
/// settings the re-apply would write, and the re-apply stands down while the
/// session owns the wheel.
#[test]
fn the_main_wheel_is_diverted_only_with_every_gate_open() {
    let mut config = Config::default();
    config.set_scroll_resolution("a", Some(ScrollResolution::High));
    config.set_invert_scroll("a", true);
    config.set_smooth_wheel("a", true);
    let wheel = Capabilities {
        hires_wheel: true,
        scroll_inversion: true,
        ..Capabilities::default()
    };
    let mut device = dev("a", 1, true);
    device.capabilities = Some(wheel);
    assert_eq!(
        main_wheel_divert(&config, &device, true),
        Some(MainWheelDivert {
            resolution: Some(ScrollResolution::High),
            inverted: Some(true),
        })
    );
    assert_eq!(
        configured_wheel_mode(&config, &device, true),
        None,
        "the capture session writes the mode with its divert"
    );

    // Without the hook nothing may post the scrolling: the wheel stays
    // native and the re-apply writes its mode as before.
    assert_eq!(main_wheel_divert(&config, &device, false), None);
    assert_eq!(
        configured_wheel_mode(&config, &device, false),
        Some(WheelModeChange::ResolutionAndInversion {
            resolution: ScrollResolution::High,
            inverted: true,
        })
    );

    config.app_settings.capture_mouse_events = false;
    assert_eq!(
        main_wheel_divert(&config, &device, true),
        None,
        "capture off"
    );
    config.app_settings.capture_mouse_events = true;

    config.set_device_enabled("a", false);
    assert_eq!(main_wheel_divert(&config, &device, true), None, "disabled");
    config.set_device_enabled("a", true);

    device.capabilities = Some(Capabilities {
        hires_wheel: false,
        ..wheel
    });
    assert_eq!(main_wheel_divert(&config, &device, true), None, "no 0x2121");
    device.capabilities = None;
    assert_eq!(main_wheel_divert(&config, &device, true), None, "unknown");
    device.capabilities = Some(wheel);

    config.set_smooth_wheel("a", false);
    assert_eq!(main_wheel_divert(&config, &device, true), None, "opted out");
}

/// The agent's own wheel-mode writes, the reconnect re-apply and the reload
/// push, leave a smooth wheel to the session that diverts it, and write its
/// mode again once the hook that posts its scrolling goes away.
#[test]
fn the_agent_writes_no_wheel_mode_under_a_diverting_session() {
    let mut config = Config::default();
    config.set_scroll_resolution("a", Some(ScrollResolution::High));
    config.set_smooth_wheel("a", true);
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        ..Capabilities::default()
    });
    let mut orch = orchestrator(config);
    orch.set_os_mouse_hook_available(true);
    assert_eq!(orch.wheel_mode_change(&device), None);
    orch.set_os_mouse_hook_available(false);
    assert_eq!(
        orch.wheel_mode_change(&device),
        Some(WheelModeChange::Resolution(ScrollResolution::High))
    );
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

    assert_eq!(configured_wheel_mode(&config, &device, true), None);
}

#[test]
fn configured_report_rate_needs_the_feature_and_a_saved_rate() {
    let mut config = Config::default();
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        report_rate: true,
        backlight: false,
        easy_switch: false,
        ..Capabilities::default()
    });
    // Nothing saved: the device keeps its own rate.
    assert_eq!(configured_report_rate(&config, &device), None);
    config.set_report_rate("a", ReportRate::new(2000));
    assert_eq!(
        configured_report_rate(&config, &device),
        Some(ReportRate::new(2000))
    );
    // A device without either report-rate feature is never written.
    device.capabilities = Some(Capabilities::default());
    assert_eq!(configured_report_rate(&config, &device), None);
    device.capabilities = None;
    assert_eq!(configured_report_rate(&config, &device), None);
}

#[test]
fn configured_force_sensing_needs_the_capability_and_a_saved_force() {
    let mut config = Config::default();
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        force_sensing: true,
        ..Capabilities::default()
    });
    // Nothing saved: the device keeps its own force.
    assert_eq!(configured_force_sensing(&config, &device), None);
    config.set_force_sensing("a", 4500);
    assert_eq!(configured_force_sensing(&config, &device), Some(4500));
    // A device whose force software may not change is never written.
    device.capabilities = Some(Capabilities::default());
    assert_eq!(configured_force_sensing(&config, &device), None);
    device.capabilities = None;
    assert_eq!(configured_force_sensing(&config, &device), None);
}

#[test]
fn a_reload_writes_a_changed_force_only_to_an_online_enabled_device() {
    let sensing = |key: &str, slot: u8, online: bool| {
        let mut device = dev(key, slot, online);
        device.capabilities = Some(Capabilities {
            force_sensing: true,
            ..Capabilities::default()
        });
        device
    };
    let devices = [
        sensing("unchanged", 1, true),
        sensing("changed", 2, true),
        sensing("disabled", 3, true),
        sensing("offline", 4, false),
        sensing("removed", 5, true),
    ];
    let mut previous = Config::default();
    previous.set_force_sensing("unchanged", 5000);
    previous.set_force_sensing("changed", 5000);
    previous.set_force_sensing("removed", 5000);
    let mut config = Config::default();
    config.set_force_sensing("unchanged", 5000);
    config.set_force_sensing("changed", 6000);
    config.set_force_sensing("disabled", 6000);
    config.set_device_enabled("disabled", false);
    config.set_force_sensing("offline", 6000);

    assert_eq!(
        changed_force_sensing(&previous, &config, &devices),
        vec![(bolt(2), 6000)],
        "a disabled device is left native, and a removed force writes nothing"
    );
}

fn bolt(slot: u8) -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: "AA00".into(),
        slot,
    }
}

/// A keyboard in `slot` whose `0x0003` unit ID is `unit`.
fn keyboard_with_unit(key: &str, slot: u8, unit: u8, online: bool) -> AgentDevice {
    let mut keyboard = dev(key, slot, online);
    keyboard.kind = openlogi_core::device::DeviceKind::Keyboard;
    keyboard.unit_id = [0, 0, 0, unit];
    keyboard
}

fn lead(unit: u8) -> openlogi_hid::LeadCookie {
    openlogi_hid::LeadCookie::for_unit_id([0, 0, 0, unit])
        .expect("the test unit IDs hash to a non-zero byte")
}

fn linking(links: &[(&str, &[&str])]) -> Config {
    let mut config = Config::default();
    for (keyboard, targets) in links {
        config
            .devices
            .entry((*keyboard).into())
            .or_default()
            .host_switch_targets = targets.iter().map(|target| (*target).into()).collect();
    }
    config
}

/// The cookie write for the device `key` in Bolt `slot`.
fn cookie_write(
    key: &str,
    slot: u8,
    release: &[openlogi_hid::LeadCookie],
    link: &[openlogi_hid::LeadCookie],
) -> FollowerCookieWrite {
    FollowerCookieWrite {
        key: key.to_owned(),
        follower: bolt(slot),
        release: release.to_vec(),
        link: link.to_vec(),
    }
}

#[test]
fn a_config_change_links_new_pairs_and_unlinks_removed_ones() {
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, false),
        dev("mouse", 2, true),
        dev("trackball", 3, true),
        dev("asleep", 4, false),
    ];
    let writes = changed_follower_cookies(
        &linking(&[("keyboard", &["mouse", "asleep"])]),
        &linking(&[("keyboard", &["trackball", "asleep"])]),
        &devices,
        &mut FollowerCookieState::default(),
    );
    assert_eq!(
        writes,
        vec![
            cookie_write("mouse", 2, &[lead(1)], &[]),
            cookie_write("trackball", 3, &[], &[lead(1)]),
        ],
        "an offline keyboard still names its byte; an unchanged pair is not written"
    );
}

#[test]
fn a_keyboard_that_left_keeps_its_last_known_byte() {
    // Its receiver is unplugged, or it is asleep and reports no unit ID.
    let mut state = FollowerCookieState::default();
    state.remember_leads(&[keyboard_with_unit("keyboard", 1, 1, true)]);
    let linked = linking(&[("keyboard", &["mouse"])]);
    for devices in [
        vec![dev("mouse", 2, true)],
        vec![dev("keyboard", 1, false), dev("mouse", 2, true)],
    ] {
        assert_eq!(
            changed_follower_cookies(&linked, &Config::default(), &devices, &mut state),
            vec![cookie_write("mouse", 2, &[lead(1)], &[])],
            "the removed link is cleared"
        );
        assert_eq!(
            changed_follower_cookies(&Config::default(), &linked, &devices, &mut state),
            vec![cookie_write("mouse", 2, &[], &[lead(1)])],
            "a new link is written"
        );
    }
}

#[test]
fn a_link_moved_to_another_keyboard_clears_the_old_byte_first() {
    // Two writes for one mouse could run in either order. The new link would
    // then be refused while the old byte is there.
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        keyboard_with_unit("other", 4, 2, true),
        dev("mouse", 2, true),
    ];
    let writes = changed_follower_cookies(
        &linking(&[("keyboard", &["mouse"])]),
        &linking(&[("other", &["mouse"])]),
        &devices,
        &mut FollowerCookieState::default(),
    );
    assert_eq!(
        writes,
        vec![cookie_write("mouse", 2, &[lead(1)], &[lead(2)])]
    );
}

#[test]
fn a_link_removed_while_the_device_is_offline_is_cleared_when_it_returns() {
    let linked = linking(&[("keyboard", &["mouse"])]);
    let unlinked = Config::default();
    let mut state = FollowerCookieState::default();
    let offline = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, false),
    ];
    assert!(changed_follower_cookies(&linked, &unlinked, &offline, &mut state).is_empty());

    let online = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, true),
    ];
    assert_eq!(
        reconnect_follower_cookies(&unlinked, &online, &online[1], &mut state),
        vec![cookie_write("mouse", 2, &[lead(1)], &[])]
    );
    assert!(
        reconnect_follower_cookies(&unlinked, &online, &online[1], &mut state).is_empty(),
        "a clear runs once"
    );
}

#[test]
fn a_pending_clear_is_dropped_when_the_link_comes_back() {
    let linked = linking(&[("keyboard", &["mouse"])]);
    let unlinked = Config::default();
    let mut state = FollowerCookieState::default();
    let offline = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, false),
    ];
    changed_follower_cookies(&linked, &unlinked, &offline, &mut state);
    changed_follower_cookies(&unlinked, &linked, &offline, &mut state);

    let online = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, true),
    ];
    assert_eq!(
        reconnect_follower_cookies(&linked, &online, &online[1], &mut state),
        vec![cookie_write("mouse", 2, &[], &[lead(1)])]
    );
}

#[test]
fn a_clear_that_did_not_finish_runs_on_the_next_reconnect() {
    // The write failed, timed out, or found the mouse gone.
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, true),
    ];
    let mut state = FollowerCookieState::default();
    state.requeue("mouse", [lead(1)]);
    assert_eq!(
        reconnect_follower_cookies(&Config::default(), &devices, &devices[1], &mut state),
        vec![cookie_write("mouse", 2, &[lead(1)], &[])]
    );
}

#[test]
fn a_clear_that_did_not_finish_leaves_a_link_made_again() {
    // The user linked the pair again before the failed clear came back.
    let config = linking(&[("keyboard", &["mouse"])]);
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, true),
    ];
    let mut state = FollowerCookieState::default();
    state.requeue("mouse", [lead(1)]);
    assert_eq!(
        reconnect_follower_cookies(&config, &devices, &devices[1], &mut state),
        vec![cookie_write("mouse", 2, &[], &[lead(1)])]
    );
}

#[test]
fn a_re_enabled_device_gets_the_link_changes_made_while_it_was_disabled() {
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        keyboard_with_unit("old", 4, 2, true),
        dev("mouse", 2, true),
    ];
    let disabled = |links: &[(&str, &[&str])]| {
        let mut config = linking(links);
        config.set_device_enabled("mouse", false);
        config
    };
    let both = disabled(&[("keyboard", &["mouse"]), ("old", &["mouse"])]);
    let one = disabled(&[("keyboard", &["mouse"])]);
    let mut state = FollowerCookieState::default();
    assert!(
        changed_follower_cookies(&both, &one, &devices, &mut state).is_empty(),
        "a disabled device gets no writes"
    );
    assert_eq!(
        changed_follower_cookies(
            &one,
            &linking(&[("keyboard", &["mouse"])]),
            &devices,
            &mut state
        ),
        vec![cookie_write("mouse", 2, &[lead(2)], &[lead(1)])],
        "the held clear runs, and the unchanged link is written"
    );
}

#[test]
fn a_byte_that_another_linked_keyboard_shares_is_not_cleared() {
    // Unit IDs 28 and 30 hash to the same byte, 0x4d.
    let devices = [
        keyboard_with_unit("keyboard", 1, 28, true),
        keyboard_with_unit("other", 4, 30, true),
        dev("mouse", 2, true),
    ];
    assert_eq!(lead(28), lead(30));
    let writes = changed_follower_cookies(
        &linking(&[("keyboard", &["mouse"]), ("other", &["mouse"])]),
        &linking(&[("other", &["mouse"])]),
        &devices,
        &mut FollowerCookieState::default(),
    );
    assert!(
        writes.is_empty(),
        "the mouse still follows the other keyboard"
    );
}

#[test]
fn cookie_writes_skip_an_unknown_keyboard_and_a_disabled_device() {
    let mut config = linking(&[("keyboard", &["mouse", "disabled"])]);
    config.set_device_enabled("disabled", false);
    let devices = [
        dev("keyboard", 1, true),
        dev("mouse", 2, true),
        dev("disabled", 3, true),
    ];
    let mut state = FollowerCookieState::default();
    assert!(
        changed_follower_cookies(&Config::default(), &config, &devices, &mut state).is_empty(),
        "a keyboard without a unit ID has no byte"
    );
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        dev("mouse", 2, true),
        dev("disabled", 3, true),
    ];
    let followers: Vec<_> =
        changed_follower_cookies(&Config::default(), &config, &devices, &mut state)
            .into_iter()
            .map(|write| write.follower)
            .collect();
    assert_eq!(followers, vec![bolt(2)], "a disabled device is left native");
}

#[test]
fn a_reconnect_links_each_pair_that_the_device_is_part_of() {
    let config = linking(&[("keyboard", &["mouse", "trackball"]), ("other", &["mouse"])]);
    let devices = [
        keyboard_with_unit("keyboard", 1, 1, true),
        keyboard_with_unit("other", 4, 2, true),
        dev("mouse", 2, true),
        dev("trackball", 3, true),
    ];
    let mut state = FollowerCookieState::default();
    assert_eq!(
        reconnect_follower_cookies(&config, &devices, &devices[2], &mut state),
        vec![cookie_write("mouse", 2, &[], &[lead(1), lead(2)])],
        "the first keyboard to write keeps the mouse; the second is refused on the device"
    );
    assert_eq!(
        reconnect_follower_cookies(&config, &devices, &devices[0], &mut state),
        vec![
            cookie_write("mouse", 2, &[], &[lead(1)]),
            cookie_write("trackball", 3, &[], &[lead(1)]),
        ]
    );
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
    // A catalogued key and a key OpenLogi has no catalog row for — both
    // stored, both diverted by their CID. A key explicitly bound to
    // `Action::None` is stored too, and is diverted just the same: presence
    // in the config is the user's explicit choice to suppress it (#1747),
    // not a reason to leave it native.
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
            (0x00e5, ButtonId::control(0x00e5)),
        ])
    );
    assert_eq!(spec.config_key, "kbd");
}

#[test]
fn keyboard_spec_is_absent_without_a_real_binding_or_for_a_disabled_keyboard() {
    let config = Config::default();
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert!(
        orch.keyboard_spec_for().is_none(),
        "an unconfigured key is never diverted"
    );

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

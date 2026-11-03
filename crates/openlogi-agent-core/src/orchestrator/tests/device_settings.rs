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

/// Momentum scrolling measures the diverted wheel's speed, so with it on for
/// the device the divert asks for high-resolution reports; off, the wheel
/// reports as before.
#[test]
fn momentum_scrolling_diverts_the_wheel_in_high_resolution() {
    let mut config = Config::default();
    config.set_smooth_wheel("a", true);
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        ..Capabilities::default()
    });
    let divert = |config: &Config| {
        main_wheel_divert(config, &device, true)
            .expect("the wheel is diverted")
            .high_resolution
    };
    assert!(!divert(&config));
    config.app_settings.momentum_scrolling.enabled = true;
    assert!(divert(&config));
    // The device's own setting wins over the app-wide default.
    config.set_device_momentum_scrolling(
        "a",
        Some(openlogi_core::config::MomentumScrolling::default()),
    );
    assert!(!divert(&config), "this mouse turned it off for itself");
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
            high_resolution: false,
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

#[test]
fn dpi_shift_returns_to_the_dpi_saved_for_the_current_link() {
    let mouse = dev("mouse", 1, true);
    let mut config = Config::default();
    config.set_dpi("mouse", Dpi::new(1600));
    config
        .devices
        .entry("mouse".to_string())
        .or_default()
        .links
        .insert(
            stable_id(&mouse).route_key(),
            openlogi_core::config::LinkConfig {
                capabilities: None,
                overrides: openlogi_core::config::LinkOverrides {
                    dpi: Some(Dpi::new(800)),
                    ..Default::default()
                },
            },
        );
    let mut orch = orchestrator(config);
    orch.devices = vec![mouse];
    orch.rebuild();
    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    let state = &dpi.by_key["mouse"];
    assert_eq!(state.committed, Some(Dpi::new(800)));
    assert_eq!(state.current, Some(Dpi::new(800)));
}

/// A `0x2201` mouse at `route` that lists 800 and 1600 DPI and keeps the
/// last DPI written. `accept` runs on each DPI write and says whether the
/// mouse takes it.
async fn publish_dpi_mouse(
    shared: &crate::orchestrator::SharedHandles,
    route: DeviceRoute,
    accept: impl Fn() -> bool + Send + Sync + 'static,
) {
    const DPI: u8 = 4;
    const INVALID_ARGUMENT: u8 = 2;
    let current = std::sync::Mutex::new(800u16);
    openlogi_device::test_support::publish_scripted_channel(
        &shared.channel_registry,
        "dpi-mouse",
        route,
        move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(request.get(..4)?);
            response[0] = 0x11;
            let mut current = current.lock().unwrap();
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => {
                    response[4] = if request[4..6] == [0x22, 0x01] {
                        DPI
                    } else {
                        0
                    }
                }
                (DPI, 1) => response[5..9].copy_from_slice(&[0x03, 0x20, 0x06, 0x40]),
                (DPI, 2) => response[5..7].copy_from_slice(&current.to_be_bytes()),
                (DPI, 3) if accept() => {
                    *current = u16::from_be_bytes([request[5], request[6]]);
                }
                (DPI, 3) => {
                    response[2..6].copy_from_slice(&[0xff, DPI, request[3], INVALID_ARGUMENT]);
                }
                _ => return None,
            }
            Some(response)
        },
    )
    .await;
}

/// A DPI written from the Pointer panel is where a DPI shift returns, even
/// when it equals the saved DPI and a reload therefore changes nothing.
#[tokio::test]
async fn a_panel_dpi_write_is_where_a_dpi_shift_returns() {
    let mut config = Config::default();
    config.set_dpi("mouse", Dpi::new(1600));
    config.set_dpi_presets("mouse", vec![Dpi::new(800), Dpi::new(1600)]);
    let mouse = dev("mouse", 1, true);
    let route = mouse.route.clone().unwrap();
    let mut orch = orchestrator(config);
    orch.devices = vec![mouse];
    orch.rebuild();
    publish_dpi_mouse(&orch.shared, route.clone(), || true).await;
    // A preset action picks 800.
    if let Ok(mut cycles) = orch.shared.dpi_cycle.write() {
        cycles.by_key.get_mut("mouse").unwrap().set(0);
    }

    orch.shared.set_dpi(&route, Dpi::new(1600)).await.unwrap();
    // The panel saves the same 1600, so the reload keeps the cached state.
    orch.rebuild();

    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    assert_eq!(dpi.by_key["mouse"].current, Some(Dpi::new(1600)));
    assert_eq!(dpi.by_key["mouse"].index, 1);
}

/// A panel write the mouse rejects is not where a DPI shift returns. The
/// mouse may be anywhere then, so the shift reads it.
#[tokio::test]
async fn a_rejected_panel_dpi_write_makes_a_dpi_shift_read_the_mouse() {
    let mut config = Config::default();
    config.set_dpi_presets("mouse", vec![Dpi::new(800), Dpi::new(1600)]);
    let mouse = dev("mouse", 1, true);
    let route = mouse.route.clone().unwrap();
    let mut orch = orchestrator(config);
    orch.devices = vec![mouse];
    orch.rebuild();
    publish_dpi_mouse(&orch.shared, route.clone(), || false).await;
    // A preset action picked 800 earlier.
    if let Ok(mut cycles) = orch.shared.dpi_cycle.write() {
        cycles.by_key.get_mut("mouse").unwrap().set(0);
    }

    orch.shared
        .set_dpi(&route, Dpi::new(1600))
        .await
        .unwrap_err();

    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    assert_eq!(dpi.by_key["mouse"].current, None);
}

/// A preset picked while a panel write runs stays, even when the mouse
/// rejects the panel write.
#[tokio::test]
async fn a_rejected_panel_dpi_write_keeps_a_preset_picked_meanwhile() {
    let mut config = Config::default();
    config.set_dpi_presets("mouse", vec![Dpi::new(800), Dpi::new(1600)]);
    let mouse = dev("mouse", 1, true);
    let route = mouse.route.clone().unwrap();
    let mut orch = orchestrator(config);
    orch.devices = vec![mouse];
    orch.rebuild();
    let cycles = Arc::clone(&orch.shared.dpi_cycle);
    publish_dpi_mouse(&orch.shared, route.clone(), move || {
        cycles
            .write()
            .unwrap()
            .by_key
            .get_mut("mouse")
            .unwrap()
            .set(0);
        false
    })
    .await;

    orch.shared
        .set_dpi(&route, Dpi::new(1600))
        .await
        .unwrap_err();

    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    assert_eq!(dpi.by_key["mouse"].current, Some(Dpi::new(800)));
}

/// A mouse that reconnects is back at its onboard DPI, so a DPI shift reads
/// it again instead of returning to a reading from before. A reload keeps
/// the reading, since the mouse did not change.
#[test]
fn a_reconnect_forgets_the_dpi_read_before_a_shift() {
    let mouse = || dev("mouse", 1, true);
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![mouse()];
    orch.rebuild();
    let reading = |orch: &Orchestrator| {
        orch.shared.dpi_cycle.read().unwrap().by_key["mouse"]
            .before_shift
            .get()
    };
    orch.shared.dpi_cycle.read().unwrap().by_key["mouse"]
        .before_shift
        .set(Dpi::new(1200));

    orch.rebuild();
    assert_eq!(reading(&orch), Some(Dpi::new(1200)));
    orch.reapply_volatile_settings(&mouse(), &[mouse()]);
    assert_eq!(reading(&orch), None);
}

#[test]
fn a_reconnect_resets_the_dpi_a_shift_returns_to() {
    // A mouse with adjustable DPI gets it through the pointer-speed write.
    for capabilities in [
        None,
        Some(Capabilities {
            pointer: true,
            ..Capabilities::default()
        }),
    ] {
        let mut config = Config::default();
        config.set_dpi("mouse", Dpi::new(1600));
        config.set_dpi_presets("mouse", vec![Dpi::new(800), Dpi::new(1600)]);
        let mouse = || AgentDevice {
            capabilities,
            ..dev("mouse", 1, true)
        };
        let mut orch = orchestrator(config);
        orch.devices = vec![mouse()];
        orch.rebuild();
        if let Ok(mut cycles) = orch.shared.dpi_cycle.write()
            && let Some(state) = cycles.by_key.get_mut("mouse")
        {
            state.set(0);
        }

        orch.reapply_volatile_settings(&mouse(), &[mouse()]);
        let Ok(dpi) = orch.shared.dpi_cycle.read() else {
            panic!("DPI cycle lock should not be poisoned");
        };
        assert_eq!(
            dpi.by_key["mouse"].current,
            Some(Dpi::new(1600)),
            "{capabilities:?}"
        );
    }
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

    let spec = sole_keyboard_spec(&orch).expect("bound keys publish a spec");
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

/// Bound keypad grid keys carry the same gate as controls but change no
/// firmware: they publish the spec — the observed set is capture identity —
/// keyed by their wire index, and an `Action::None` one is left native like
/// any other unbound key.
#[test]
fn grid_only_bindings_publish_their_index_set() {
    let mut config = Config::default();
    config.set_binding(
        "keypad",
        ButtonId::grid(GridKey::Middle),
        Binding::Single(Action::Screenshot),
    );
    config.set_binding(
        "keypad",
        ButtonId::grid(GridKey::BottomRight),
        Binding::Single(Action::None),
    );
    let mut orch = orchestrator(config);
    let mut keypad = dev("keypad", 1, true);
    keypad.kind = DeviceKind::Numpad;
    orch.devices = vec![keypad];

    let spec = sole_keyboard_spec(&orch).expect("the bound grid key publishes a spec");
    assert_eq!(
        spec.grid,
        // `Middle` is the vendor report's index 5.
        BTreeMap::from([(5, ButtonId::grid(GridKey::Middle))])
    );
    assert!(spec.wanted.is_empty(), "grid keys divert nothing");
}

#[test]
fn keyboard_spec_is_absent_without_a_real_binding_or_for_a_disabled_keyboard() {
    let config = Config::default();
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, true)];
    assert!(
        sole_keyboard_spec(&orch).is_none(),
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
        sole_keyboard_spec(&orch).is_none(),
        "a disabled keyboard is left fully native"
    );
}

/// Every bound keyboard-family device captures at once, each on its own
/// route with its own keys and bindings — asleep or not — so a keyboard and
/// a keypad never compete for one session.
#[test]
fn every_bound_keyboard_publishes_its_own_spec() {
    let mut config = Config::default();
    config.set_binding(
        "kbd",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    config.set_binding(
        "keypad",
        ButtonId::grid(GridKey::Middle),
        Binding::Single(Action::Copy),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![keyboard("kbd", 1, false), wired_keypad("keypad", true)];

    let specs = orch.keyboard_specs_for();
    let summary: Vec<_> = specs
        .iter()
        .map(|spec| {
            (
                spec.config_key.as_str(),
                spec.wanted.keys().copied().collect::<Vec<_>>(),
                spec.grid.keys().copied().collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [("kbd", vec![0x010a], vec![]), ("keypad", vec![], vec![5])]
    );
    assert_ne!(specs[0].route, specs[1].route);
    assert_eq!(
        specs[1].bindings.get(&ButtonId::grid(GridKey::Middle)),
        Some(&Binding::Single(Action::Copy)),
        "each spec dispatches its own device's bindings"
    );
}

/// Two inventory entries on one route — a direct route names only a VID/PID —
/// still reach one device, so they publish one spec: the online entry's, and
/// with both asleep the first in inventory order.
#[test]
fn entries_sharing_a_route_publish_one_spec_preferring_the_online_one() {
    let mut config = Config::default();
    for key in ["first", "second"] {
        config.set_binding(
            key,
            ButtonId::grid(GridKey::Middle),
            Binding::Single(Action::Copy),
        );
    }
    let mut orch = orchestrator(config);
    orch.devices = vec![wired_keypad("first", false), wired_keypad("second", true)];
    assert_eq!(
        sole_keyboard_spec(&orch).expect("spec").config_key,
        "second"
    );

    orch.devices = vec![wired_keypad("first", false), wired_keypad("second", false)];
    assert_eq!(sole_keyboard_spec(&orch).expect("spec").config_key, "first");
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
    assert_eq!(sole_keyboard_spec(&orch).expect("spec").config_key, "bound");

    // An unbound keyboard being online does not make it a candidate: the
    // bound one holds the session even while asleep.
    orch.devices = vec![keyboard("unbound", 1, true), keyboard("bound", 2, false)];
    assert_eq!(sole_keyboard_spec(&orch).expect("spec").config_key, "bound");
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
    assert_eq!(sole_keyboard_spec(&orch).expect("spec").config_key, "live");
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
    assert!(sole_keyboard_spec(&orch).is_none());

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
        sole_keyboard_spec(&orch).expect("spec").wanted,
        BTreeMap::from([(0x010a, ButtonId::control(0x010a))])
    );
}

/// The registry's MX Creative Keypad, wired: a route the LCD lookup knows.
fn wired_keypad(key: &str, online: bool) -> AgentDevice {
    let keypad = &openlogi_device_registry::keypad::KEYPADS[0];
    AgentDevice {
        route: Some(DeviceRoute::Direct {
            vendor_id: keypad.vendor_id,
            product_id: keypad.product_id,
        }),
        slot: DIRECT_DEVICE_INDEX,
        kind: DeviceKind::Numpad,
        ..dev(key, DIRECT_DEVICE_INDEX, online)
    }
}

/// Each grid key shows its bound action's glyph, and a key that does nothing
/// — unbound, or bound to `None` — shows nothing: the same "bound" rule the
/// capture set uses, so a lit key always does something.
#[test]
fn keypad_key_faces_follow_the_bound_actions() {
    use crate::watchers::key_display::KeyFace;
    use openlogi_core::binding::ActionRingIcon;

    let mut config = Config::default();
    config.set_binding(
        "keypad",
        ButtonId::grid(GridKey::Middle),
        Binding::Single(Action::Copy),
    );
    config.set_binding(
        "keypad",
        ButtonId::grid(GridKey::BottomRight),
        Binding::Single(Action::None),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![wired_keypad("keypad", true)];

    let specs = orch.key_display_specs_for();
    let [spec] = specs.as_slice() else {
        panic!("one keypad publishes one spec: {specs:?}");
    };
    assert_eq!(spec.faces.len(), 9);
    assert_eq!(
        spec.faces[&GridKey::Middle],
        KeyFace::Icon(ActionRingIcon::for_action(&Action::Copy))
    );
    assert_eq!(spec.faces[&GridKey::BottomRight], KeyFace::Blank);
    assert_eq!(spec.faces[&GridKey::TopLeft], KeyFace::Blank);
    assert_eq!(spec.display.key_size, 118);
}

/// Only a device whose panel the registry describes gets a spec — a bound
/// numpad-kind device on another route does not — and a keypad that is
/// offline or disabled publishes nothing to paint.
#[test]
fn key_display_specs_need_a_present_enabled_display_keypad() {
    let mut config = Config::default();
    config.set_binding(
        "other",
        ButtonId::grid(GridKey::Middle),
        Binding::Single(Action::Copy),
    );
    let mut orch = orchestrator(config);
    let mut other = dev("other", 1, true);
    other.kind = DeviceKind::Numpad;
    orch.devices = vec![other, wired_keypad("asleep", false)];
    assert!(orch.key_display_specs_for().is_empty());

    let mut config = Config::default();
    config.set_device_enabled("keypad", false);
    let mut orch = orchestrator(config);
    orch.devices = vec![wired_keypad("keypad", true)];
    assert!(orch.key_display_specs_for().is_empty());
}

/// The wired keypad as the enumerator reports it, so a test drives the
/// orchestrator's own reconnect handling through inventory refreshes.
fn keypad_inventory(online: bool) -> DeviceInventory {
    let keypad = &openlogi_device_registry::keypad::KEYPADS[0];
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.name = "MX Creative Keypad".to_owned();
    inventory.receiver.product_id = keypad.product_id;
    let device = &mut inventory.paired[0];
    device.codename = Some("MX Creative Keypad".to_owned());
    device.kind = DeviceKind::Numpad;
    device.online = online;
    device.capabilities = Some(Capabilities::presumed_from_kind(DeviceKind::Numpad));
    inventory
}

fn panel_generation(orch: &Orchestrator) -> u64 {
    let specs = orch.key_display_specs_for();
    let [spec] = specs.as_slice() else {
        panic!("one keypad publishes one spec: {specs:?}");
    };
    spec.generation
}

/// The panel repaints whenever the keypad's volatile settings re-apply: on
/// coming back online, and on a reconnect the snapshot cannot show — its own
/// `0x1d4b` broadcast or a replaced channel, which arrive as a forced
/// re-apply. A steady snapshot and its confirming re-applies repaint nothing.
#[test]
fn every_keypad_reconnect_moves_its_panel_generation() {
    let mut orch = orchestrator(Config::default());
    let online = [keypad_inventory(true)];
    orch.refresh_inventory(&online, &[], false);
    let first = panel_generation(&orch);

    orch.refresh_inventory(&online, &[], false);
    assert_eq!(panel_generation(&orch), first, "a steady snapshot");

    orch.reapply_reconnected_on_next_refresh();
    orch.refresh_inventory(&online, &[], false);
    let announced = panel_generation(&orch);
    assert_ne!(announced, first, "an announced reconnect repaints");

    orch.refresh_inventory(&[keypad_inventory(false)], &[], false);
    assert!(orch.key_display_specs_for().is_empty(), "nothing to paint");
    orch.refresh_inventory(&online, &[], false);
    assert_ne!(
        panel_generation(&orch),
        announced,
        "an observed reconnect repaints"
    );
}

/// Another device coming online moves only its own epoch, so the keypad
/// keeps what it shows.
#[test]
fn another_device_reconnecting_leaves_the_keypad_painted() {
    let mut orch = orchestrator(Config::default());
    let mut mouse = direct_inventory(Some("mouse"), [9, 9, 9, 9]);
    mouse.paired[0].online = false;
    orch.refresh_inventory(&[keypad_inventory(true), mouse.clone()], &[], false);
    let before = panel_generation(&orch);

    mouse.paired[0].online = true;
    orch.refresh_inventory(&[keypad_inventory(true), mouse], &[], false);

    assert_eq!(panel_generation(&orch), before);
}

/// With more than one page the paging keys turn the page by default, so a
/// keypad with nothing else bound still needs its capture session for them.
#[test]
fn a_paged_keypad_with_nothing_bound_captures_its_paging_keys() {
    let mut config = Config::default();
    config.set_keypad_pages("keypad", page(2));
    let mut orch = orchestrator(config);
    orch.devices = vec![wired_keypad("keypad", true)];

    let spec = sole_keyboard_spec(&orch).expect("the paging keys hold a session");
    let [left, right] = openlogi_core::binding::Cid::PAGING;
    assert_eq!(
        spec.wanted.keys().copied().collect::<Vec<_>>(),
        [left.raw(), right.raw()]
    );
    assert_eq!(
        spec.bindings[&ButtonId::Control(right)],
        Binding::Single(Action::NextKeypadPage)
    );

    orch.config.set_keypad_pages("keypad", page(1));
    assert!(
        sole_keyboard_spec(&orch).is_none(),
        "one page leaves the paging keys native"
    );
}

fn page(number: u8) -> openlogi_core::binding::KeypadPage {
    openlogi_core::binding::KeypadPage::new(number).expect("a valid page")
}

fn on_page(number: u8, key: GridKey) -> ButtonId {
    ButtonId::Grid(openlogi_core::binding::GridSlot {
        page: page(number),
        key,
    })
}

/// A two-page keypad: Copy on page 1's middle key, Paste on page 2's middle
/// and Undo on page 2's top-left.
fn paged_keypad() -> Orchestrator {
    let mut config = Config::default();
    config.set_keypad_pages("keypad", page(2));
    config.set_binding(
        "keypad",
        on_page(1, GridKey::Middle),
        Binding::Single(Action::Copy),
    );
    config.set_binding(
        "keypad",
        on_page(2, GridKey::Middle),
        Binding::Single(Action::Paste),
    );
    config.set_binding(
        "keypad",
        on_page(2, GridKey::TopLeft),
        Binding::Single(Action::Undo),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![wired_keypad("keypad", true)];
    orch
}

/// The capture session observes every key bound on any page, so turning the
/// page swaps only what a press dispatches — the capture identity, and with
/// it the running session, stays put.
#[test]
fn a_page_turn_swaps_dispatch_without_changing_the_observed_keys() {
    use crate::runtime::PageTurn;
    let mut orch = paged_keypad();

    let first = sole_keyboard_spec(&orch).expect("bound keys publish a spec");
    assert_eq!(
        first.grid,
        BTreeMap::from([
            (1, ButtonId::grid(GridKey::TopLeft)),
            (5, ButtonId::grid(GridKey::Middle)),
        ])
    );
    assert_eq!(
        first.bindings.get(&ButtonId::grid(GridKey::Middle)),
        Some(&Binding::Single(Action::Copy))
    );
    assert!(
        !first
            .bindings
            .contains_key(&ButtonId::grid(GridKey::TopLeft))
    );
    // Two pages: the paging buttons are bound to the page actions.
    assert_eq!(
        first.bindings.get(&ButtonId::Control(
            openlogi_core::binding::Cid::RIGHT_PAGING
        )),
        Some(&Binding::Single(Action::NextKeypadPage))
    );

    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    let second = sole_keyboard_spec(&orch).expect("spec");
    assert_eq!(
        second.grid, first.grid,
        "the observed set is page-independent"
    );
    assert_eq!(second.wanted, first.wanted);
    assert_eq!(
        second.bindings.get(&ButtonId::grid(GridKey::Middle)),
        Some(&Binding::Single(Action::Paste))
    );
    assert_eq!(
        second.bindings.get(&ButtonId::grid(GridKey::TopLeft)),
        Some(&Binding::Single(Action::Undo))
    );
}

#[test]
fn the_lcd_follows_the_current_page_and_turns_wrap() {
    use crate::runtime::PageTurn;
    use crate::watchers::key_display::KeyFace;
    use openlogi_core::binding::ActionRingIcon;
    let mut orch = paged_keypad();
    let face = |orch: &Orchestrator, key| orch.key_display_specs_for()[0].faces[&key];
    let icon = |action: Action| KeyFace::Icon(ActionRingIcon::for_action(&action));

    assert_eq!(face(&orch, GridKey::Middle), icon(Action::Copy));
    assert_eq!(face(&orch, GridKey::TopLeft), KeyFace::Blank);

    orch.turn_keypad_page(Some("keypad"), PageTurn::Previous);
    assert_eq!(
        face(&orch, GridKey::Middle),
        icon(Action::Paste),
        "1 wraps back to 2"
    );
    assert_eq!(face(&orch, GridKey::TopLeft), icon(Action::Undo));

    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    assert_eq!(
        face(&orch, GridKey::Middle),
        icon(Action::Copy),
        "2 wraps on to 1"
    );
}

/// A single-page device has nothing to turn, a turn with no device is
/// dropped, and a page the config has since removed reads as page 1.
#[test]
fn page_turns_need_pages_and_a_removed_page_falls_back_to_the_first() {
    use crate::runtime::PageTurn;
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![wired_keypad("keypad", true)];
    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    orch.turn_keypad_page(None, PageTurn::Next);
    assert_eq!(orch.keypad_page("keypad"), page(1));

    let mut orch = paged_keypad();
    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    assert_eq!(orch.keypad_page("keypad"), page(2));
    orch.config.set_keypad_pages("keypad", page(1));
    assert_eq!(orch.keypad_page("keypad"), page(1));
}

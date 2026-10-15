//! Which keyboards the agent keeps on the host OS's `0x4531` key layout, and
//! when.

use super::*;

/// An online keyboard whose platform software can set.
fn layout_keyboard(key: &str, slot: u8) -> AgentDevice {
    let mut device = dev(key, slot, true);
    device.kind = DeviceKind::Keyboard;
    device.capabilities = Some(Capabilities {
        os_layout: true,
        ..Capabilities::default()
    });
    device
}

#[test]
fn keeps_the_layout_of_a_settable_keyboard_by_default() {
    let config = Config::default();
    assert!(keeps_os_layout(&config, &layout_keyboard("kbd", 1)));
    // Not without the capability, or with unknown capabilities.
    let mut no_capability = layout_keyboard("kbd", 1);
    no_capability.capabilities = Some(Capabilities::default());
    assert!(!keeps_os_layout(&config, &no_capability));
    no_capability.capabilities = None;
    assert!(!keeps_os_layout(&config, &no_capability));
}

#[test]
fn turning_the_setting_off_or_disabling_the_device_stops_it() {
    let keyboard = layout_keyboard("kbd", 1);
    let mut off = Config::default();
    off.set_keep_os_layout("kbd", false);
    assert!(!keeps_os_layout(&off, &keyboard));
    let mut disabled = Config::default();
    disabled.set_device_enabled("kbd", false);
    assert!(!keeps_os_layout(&disabled, &keyboard));
    // Explicitly on is the same as never set.
    let mut on = Config::default();
    on.set_keep_os_layout("kbd", true);
    assert!(keeps_os_layout(&on, &keyboard));
}

#[test]
fn a_reload_writes_only_the_keyboards_whose_setting_just_took_effect() {
    let devices = [layout_keyboard("kbd", 1), layout_keyboard("other", 2)];
    let keys = |previous: &Config, config: &Config| {
        newly_kept_os_layouts(previous, config, &devices)
            .map(|dev| dev.config_key.clone())
            .collect::<Vec<_>>()
    };
    let mut off = Config::default();
    off.set_keep_os_layout("kbd", false);
    let mut disabled = Config::default();
    disabled.set_device_enabled("kbd", false);

    // Turned on, or the device re-enabled: write.
    assert_eq!(keys(&off, &Config::default()), ["kbd"]);
    assert_eq!(keys(&disabled, &Config::default()), ["kbd"]);
    // An unrelated save, or turning it off: no write. Off leaves the keyboard
    // on whatever layout it has.
    assert!(keys(&Config::default(), &Config::default()).is_empty());
    assert!(keys(&Config::default(), &off).is_empty());
}

/// `layout_keyboard("kbd", slot)` with `online` and `settings_revision`.
fn kbd(slot: u8, online: bool, settings_revision: u64) -> AgentDevice {
    AgentDevice {
        online,
        settings_revision,
        ..layout_keyboard("kbd", slot)
    }
}

#[test]
fn a_device_side_setting_change_is_seen_on_the_same_route_only() {
    assert_eq!(
        settings_moved(&[kbd(1, true, 0)], &[kbd(1, true, 1)]),
        vec![0]
    );
    // An unchanged revision, a first sighting, and a device that just came
    // online (a reapply covers it) are not.
    assert!(settings_moved(&[kbd(1, true, 0)], &[kbd(1, true, 0)]).is_empty());
    assert!(settings_moved(&[], &[kbd(1, true, 1)]).is_empty());
    assert!(settings_moved(&[kbd(1, false, 0)], &[kbd(1, true, 1)]).is_empty());
    // An offline device is not written.
    assert!(settings_moved(&[kbd(1, true, 0)], &[kbd(1, false, 1)]).is_empty());
    // The revision restarts with a new channel, so another route compares
    // nothing — the replug re-applies instead.
    assert!(settings_moved(&[kbd(1, true, 1)], &[kbd(2, true, 0)]).is_empty());
}

#[test]
fn build_devices_carries_the_settings_revision() {
    let mut inventory = direct_inventory(Some("SERIAL"), [1, 2, 3, 4]);
    inventory.paired[0].settings_revision = 5;
    let devices = build_devices(&Config::default(), &[inventory], &[]);
    assert_eq!(devices[0].settings_revision, 5);
}

/// `kbd(slot, true, 0)` with `capabilities`.
fn kbd_with(slot: u8, online: bool, capabilities: Option<Capabilities>) -> AgentDevice {
    AgentDevice {
        capabilities,
        ..kbd(slot, online, 0)
    }
}

#[test]
fn os_layout_support_found_on_an_online_route_is_checked() {
    let layout = Some(Capabilities {
        os_layout: true,
        ..Capabilities::default()
    });
    let without = Some(Capabilities::default());
    // Unknown capabilities (a failed feature walk) or a walk without the
    // feature, then the feature: check.
    assert_eq!(
        os_layout_appeared(&[kbd_with(1, true, None)], &[kbd_with(1, true, layout)]),
        vec![0]
    );
    assert_eq!(
        os_layout_appeared(&[kbd_with(1, true, without)], &[kbd_with(1, true, layout)]),
        vec![0]
    );
    // Already known, still unknown, offline, a first sighting, a device that
    // just came online and a new route are not: a reapply covers those.
    assert!(
        os_layout_appeared(&[kbd_with(1, true, layout)], &[kbd_with(1, true, layout)]).is_empty()
    );
    assert!(os_layout_appeared(&[kbd_with(1, true, None)], &[kbd_with(1, true, None)]).is_empty());
    assert!(
        os_layout_appeared(&[kbd_with(1, true, None)], &[kbd_with(1, false, layout)]).is_empty()
    );
    assert!(os_layout_appeared(&[], &[kbd_with(1, true, layout)]).is_empty());
    assert!(
        os_layout_appeared(&[kbd_with(1, false, None)], &[kbd_with(1, true, layout)]).is_empty()
    );
    assert!(
        os_layout_appeared(&[kbd_with(1, true, None)], &[kbd_with(2, true, layout)]).is_empty()
    );
}

#[test]
fn a_reload_withdraws_queued_layout_writes_the_user_opted_out_of() {
    let mut orch = orchestrator(Config::default());
    let kept = Arc::clone(&orch.kept_os_layouts);
    for key in ["kbd", "disabled", "other"] {
        kept.keep(key);
    }
    let mut config = Config::default();
    config.set_keep_os_layout("kbd", false);
    config.set_device_enabled("disabled", false);
    orch.reload_config(config);
    assert!(!kept.contains("kbd"));
    assert!(!kept.contains("disabled"));
    assert!(kept.contains("other"));
}

#[test]
fn a_refresh_checks_a_keyboard_whose_layout_support_shows_up_late() {
    let mut orch = orchestrator(Config::default());
    // A walk that failed: online, capabilities unknown.
    let mut inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);
    inventory.paired[0].kind = DeviceKind::Keyboard;
    inventory.paired[0].capabilities = None;
    orch.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
        orch.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    }
    assert!(
        !orch.needs_reapply_confirmation(),
        "sanity: no reapply left"
    );
    let key = orch.devices[0].config_key.clone();
    assert!(!orch.kept_os_layouts.contains(&key));
    // A later walk finds `0x4531` on the same, still-online route.
    inventory.paired[0].capabilities = Some(Capabilities {
        os_layout: true,
        ..Capabilities::default()
    });
    orch.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(orch.kept_os_layouts.contains(&key));
}

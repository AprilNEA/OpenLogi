use super::*;
use crate::binding::Action;

/// An MX Master 3S set up the way a user sets one up.
const SOURCE: &str = r#"
dpi = 1000
thumbwheel_sensitivity = 90
scroll_resolution = "low"

[bindings.DpiToggle]
Up = "NewTab"
Down = "CloseTab"
Left = "PrevTab"
Right = "NextTab"
Click = "MissionControl"

[bindings.GestureButton]
Up = "AppExpose"
Down = "ShowDesktop"
Left = "NextDesktop"
Right = "PreviousDesktop"
Click = "MissionControl"

[smartshift]
mode = "ratchet"
auto_disengage = 15
tunable_torque = 0
"#;

fn device(toml: &str) -> DeviceConfig {
    toml::from_str(toml).expect("a valid device table")
}

fn mouse(capabilities: Capabilities) -> TransferTarget {
    TransferTarget {
        kind: Some(DeviceKind::Mouse),
        capabilities: Some(capabilities),
    }
}

/// What an MX Master 3 reports.
fn mx_master() -> Capabilities {
    Capabilities {
        buttons: true,
        pointer: true,
        scroll_inversion: true,
        hires_wheel: true,
        thumbwheel: true,
        dpi_gestures: true,
        ..Capabilities::default()
    }
}

#[test]
fn a_new_mouse_takes_every_setting_the_old_one_has() {
    let transfer = transfer_settings(
        &device(SOURCE),
        &DeviceConfig::default(),
        mouse(mx_master()),
    );

    assert_eq!(
        transfer.changed,
        [
            TransferItem::Button(ButtonId::DpiToggle),
            TransferItem::Button(ButtonId::GestureButton),
            TransferItem::ThumbwheelSensitivity,
            TransferItem::ScrollResolution,
            TransferItem::SmartShift,
            TransferItem::Dpi,
        ]
    );
    assert!(transfer.skipped.is_empty(), "{:?}", transfer.skipped);
    let source = device(SOURCE);
    assert_eq!(transfer.settings.bindings, source.bindings);
    assert_eq!(transfer.settings.dpi, source.dpi);
    assert_eq!(transfer.settings.smartshift, source.smartshift);
}

#[test]
fn transferring_again_changes_nothing() {
    let first = transfer_settings(
        &device(SOURCE),
        &DeviceConfig::default(),
        mouse(mx_master()),
    );
    let again = transfer_settings(&device(SOURCE), &first.settings, mouse(mx_master()));

    assert!(again.is_empty(), "{:?}", again.changed);
    assert_eq!(again.settings, first.settings);
}

#[test]
fn the_targets_other_buttons_keep_their_bindings() {
    let target = device(
        r#"
        dpi = 1600
        [bindings]
        Back = "Copy"
        "#,
    );
    let transfer = transfer_settings(&device(SOURCE), &target, mouse(mx_master()));

    assert_eq!(
        transfer.settings.bindings.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
    assert_eq!(
        transfer.settings.dpi,
        device(SOURCE).dpi,
        "the source's DPI wins"
    );
}

#[test]
fn a_setting_the_source_never_set_leaves_the_targets_alone() {
    let target = device("pointer_speed = 120\nthumbwheel_sensitivity = 40");
    let source = device("dpi = 1000");
    let transfer = transfer_settings(&source, &target, mouse(mx_master()));

    assert_eq!(transfer.changed, [TransferItem::Dpi]);
    assert_eq!(transfer.settings.pointer_speed, target.pointer_speed);
    assert_eq!(
        transfer.settings.thumbwheel_sensitivity,
        target.thumbwheel_sensitivity
    );
}

#[test]
fn controls_the_target_lacks_are_left_out_and_reported() {
    let source = device(
        r#"
        thumbwheel_sensitivity = 90
        [bindings]
        HapticPanel = "Copy"
        Thumbwheel = "Paste"
        [bindings.DpiToggle]
        Up = "NewTab"
        Click = "MissionControl"
        "#,
    );
    // An older mouse: no thumb wheel, no haptic panel, no DPI-button gestures.
    let capabilities = Capabilities {
        buttons: true,
        pointer: true,
        ..Capabilities::default()
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), mouse(capabilities));

    assert!(transfer.is_empty(), "{:?}", transfer.changed);
    assert_eq!(
        transfer.skipped,
        [
            SkippedItem {
                item: TransferItem::Button(ButtonId::DpiToggle),
                reason: SkipReason::NoDpiGestures,
            },
            SkippedItem {
                item: TransferItem::Button(ButtonId::Thumbwheel),
                reason: SkipReason::NoThumbwheel,
            },
            SkippedItem {
                item: TransferItem::Button(ButtonId::HapticPanel),
                reason: SkipReason::NoHapticPanel,
            },
            SkippedItem {
                item: TransferItem::ThumbwheelSensitivity,
                reason: SkipReason::NoThumbwheel,
            },
        ]
    );
    assert_eq!(transfer.settings, DeviceConfig::default());
}

#[test]
fn a_plain_dpi_button_binding_needs_no_gestures() {
    let source = device("[bindings]\nDpiToggle = \"MissionControl\"");
    let capabilities = Capabilities {
        buttons: true,
        ..Capabilities::default()
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), mouse(capabilities));

    assert_eq!(
        transfer.changed,
        [TransferItem::Button(ButtonId::DpiToggle)]
    );
}

#[test]
fn keyboard_keys_never_land_on_a_mouse() {
    let source = device("[bindings]\n\"control:0x0141\" = \"Copy\"");
    let transfer = transfer_settings(&source, &DeviceConfig::default(), mouse(mx_master()));

    assert!(transfer.is_empty());
    assert_eq!(
        transfer.skipped.first().map(|skip| skip.reason),
        Some(SkipReason::NotOnAMouse)
    );
}

#[test]
fn unknown_capabilities_leave_nothing_out() {
    let source = device("[bindings]\nHapticPanel = \"Copy\"\nThumbwheel = \"Paste\"");
    let target = TransferTarget {
        kind: Some(DeviceKind::Mouse),
        capabilities: None,
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), target);

    assert_eq!(transfer.changed.len(), 2);
    assert!(transfer.skipped.is_empty());
}

#[test]
fn a_copied_value_replaces_the_targets_link_override() {
    let target = device(
        r#"
        dpi = 1000
        [links."direct:046d:b023".overrides]
        dpi = 2400
        "#,
    );
    let transfer = transfer_settings(&device("dpi = 1000"), &target, mouse(mx_master()));

    assert_eq!(
        transfer.changed,
        [TransferItem::Dpi],
        "the override shadowed it"
    );
    let link = &transfer.settings.links["direct:046d:b023"];
    assert_eq!(link.overrides.dpi, None);
}

#[test]
fn a_matching_app_override_adds_no_empty_profile() {
    let source = device("[per_app_bindings.\"com.apple.Safari\"]\nBack = \"Copy\"");
    let first = transfer_settings(&source, &DeviceConfig::default(), mouse(mx_master()));
    assert_eq!(
        first.changed,
        [TransferItem::AppButton {
            app: "com.apple.Safari".to_owned(),
            button: ButtonId::Back,
        }]
    );

    let skipped_app = device("[per_app_bindings.\"com.apple.Safari\"]\nHapticPanel = \"Copy\"");
    let old_mouse = mouse(Capabilities {
        buttons: true,
        ..Capabilities::default()
    });
    let transfer = transfer_settings(&skipped_app, &DeviceConfig::default(), old_mouse);
    assert!(transfer.settings.per_app_bindings.is_empty());
}

#[test]
fn the_config_transfers_between_its_own_devices_and_applies_it() {
    let mut config = Config::default();
    config
        .devices
        .insert("serial:old".to_owned(), device(SOURCE));
    let transfer = config
        .settings_transfer("serial:old", "unit:new", mouse(mx_master()))
        .expect("the source has an entry");
    assert_eq!(transfer.changed.len(), 6);

    config.apply_settings_transfer("unit:new", transfer);

    let again = config
        .settings_transfer("serial:old", "unit:new", mouse(mx_master()))
        .expect("the source has an entry");
    assert!(again.is_empty());
    assert!(
        config
            .settings_transfer("serial:missing", "unit:new", mouse(mx_master()))
            .is_none()
    );
}

#[test]
fn an_export_holds_one_device_without_this_machines_routes() {
    let mut config = Config::default();
    let mut source = device(SOURCE);
    source.links.insert(
        "direct:046d:b034".to_owned(),
        crate::config::LinkConfig::default(),
    );
    source
        .host_switch_targets
        .push("serial:keyboard".to_owned());
    config.devices.insert("serial:old".to_owned(), source);
    config
        .devices
        .insert("serial:other".to_owned(), DeviceConfig::default());

    let export = config
        .device_export("serial:old")
        .expect("the device has an entry");
    let text = toml::to_string_pretty(&export).expect("a config serializes");
    let imported: Config = toml::from_str(&text).expect("the export reads back");

    assert!(!text.contains("serial:"), "no hardware identifier: {text}");
    assert_eq!(imported.devices.len(), 1);
    let device = imported.devices.values().next().expect("one device");
    assert!(device.links.is_empty());
    assert!(device.host_switch_targets.is_empty());
    assert_eq!(device.bindings, config.devices["serial:old"].bindings);
    assert!(config.device_export("serial:missing").is_none());
}

/// A device whose buttons, Actions Ring and app profile run commands and
/// scripts, next to bindings that run none.
const RUNS_CODE: &str = r#"
[bindings]
Back = { RunShellCommand = "say back" }
Forward = { Workflow = [{ TypeText = "hi" }, { RunAppleScript = "beep" }] }
Thumbwheel = { Workflow = [{ TypeText = "hi" }] }

[bindings.GestureButton]
Up = { RunShellCommand = "say up" }
Click = "MissionControl"

[disabled_gestures.DpiToggle]
Up = { RunAppleScript = "beep" }

[per_app_bindings."com.apple.Safari"]
Back = { RunShellCommand = "say safari" }
Forward = "Copy"

[action_ring.default.slots]
Top = { action = { RunShellCommand = "say ring" } }
"#;

#[test]
fn a_file_brings_no_command_or_script() {
    let config = Config::default();
    let transfer =
        config.settings_transfer_from_file(&device(RUNS_CODE), "unit:new", mouse(mx_master()));

    assert_eq!(transfer.changed.len(), 2, "{:?}", transfer.changed);
    assert!(
        transfer
            .changed
            .contains(&TransferItem::Button(ButtonId::Thumbwheel))
    );
    assert!(transfer.changed.contains(&TransferItem::AppButton {
        app: "com.apple.Safari".to_owned(),
        button: ButtonId::Forward,
    }));
    let left_out: Vec<&TransferItem> = transfer
        .skipped
        .iter()
        .inspect(|skipped| assert_eq!(skipped.reason, SkipReason::RunsCode))
        .map(|skipped| &skipped.item)
        .collect();
    for item in [
        TransferItem::Button(ButtonId::Back),
        TransferItem::Button(ButtonId::Forward),
        TransferItem::Button(ButtonId::GestureButton),
        TransferItem::StoredGestures(ButtonId::DpiToggle),
        TransferItem::AppButton {
            app: "com.apple.Safari".to_owned(),
            button: ButtonId::Back,
        },
        TransferItem::ActionRing,
    ] {
        assert!(left_out.contains(&&item), "{item:?} left out: {left_out:?}");
    }
    assert_eq!(left_out.len(), 6);

    let settings = &transfer.settings;
    assert!(!settings.bindings.contains_key(&ButtonId::Back));
    assert!(!settings.bindings.contains_key(&ButtonId::GestureButton));
    assert!(settings.disabled_gestures.is_empty());
    assert!(settings.action_ring.is_default());
}

#[test]
fn a_device_of_this_machine_brings_its_commands_and_scripts() {
    let transfer = transfer_settings(
        &device(RUNS_CODE),
        &DeviceConfig::default(),
        mouse(mx_master()),
    );

    assert!(transfer.skipped.is_empty(), "{:?}", transfer.skipped);
    assert_eq!(transfer.changed.len(), 8, "{:?}", transfer.changed);
    assert_eq!(transfer.settings.bindings, device(RUNS_CODE).bindings);
    assert_eq!(transfer.settings.action_ring, device(RUNS_CODE).action_ring);
}

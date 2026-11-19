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
        invertible_wheel: capabilities.scroll_inversion,
    }
}

/// The device `toml` describes, read from a file that defines nothing else.
fn imported(toml: &str) -> ImportedDevice {
    let mut file = Config::default();
    file.devices.insert(EXPORT_KEY.to_owned(), device(toml));
    file.imported_device(EXPORT_KEY)
        .expect("the file has the device")
}

fn config(toml: &str) -> Config {
    let version = crate::config::SCHEMA_VERSION;
    toml::from_str(&format!("schema_version = {version}\n{toml}")).expect("a valid config")
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
fn unmeasured_controls_leave_every_hardware_setting_out() {
    let source = device(
        r#"
        dpi = 1000
        dpi_presets = [800, 1600]
        pointer_speed = 120
        thumbwheel_sensitivity = 90
        scroll_resolution = "low"
        invert_scroll = true
        smooth_wheel = true
        [bindings]
        Back = "Copy"
        HapticPanel = "Copy"
        Thumbwheel = "Paste"
        [bindings.DpiToggle]
        Up = "NewTab"
        Click = "MissionControl"
        [smartshift]
        mode = "ratchet"
        auto_disengage = 15
        tunable_torque = 0
        "#,
    );
    // Even a caller that would offer inversion cannot vouch for a mouse
    // whose controls were never read.
    let target = TransferTarget {
        kind: Some(DeviceKind::Mouse),
        capabilities: None,
        invertible_wheel: true,
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), target);

    assert_eq!(
        transfer.changed,
        [TransferItem::Button(ButtonId::Back)],
        "a plain button needs no measured control"
    );
    let left_out: Vec<&TransferItem> = transfer
        .skipped
        .iter()
        .inspect(|skipped| assert_eq!(skipped.reason, SkipReason::NotMeasured, "{skipped:?}"))
        .map(|skipped| &skipped.item)
        .collect();
    assert_eq!(
        left_out,
        [
            &TransferItem::Button(ButtonId::DpiToggle),
            &TransferItem::Button(ButtonId::Thumbwheel),
            &TransferItem::Button(ButtonId::HapticPanel),
            &TransferItem::ThumbwheelSensitivity,
            &TransferItem::InvertScroll,
            &TransferItem::ScrollResolution,
            &TransferItem::SmoothWheel,
            &TransferItem::SmartShift,
            &TransferItem::Dpi,
            &TransferItem::DpiPresets,
            &TransferItem::PointerSpeed,
        ]
    );
    assert_eq!(transfer.settings.dpi, None);
    assert_eq!(transfer.settings.smartshift, None);
}

#[test]
fn a_source_that_only_binds_a_button_keeps_the_targets_scroll_options() {
    let target = device(
        r#"
        invert_scroll = true
        smooth_wheel = true
        [links."direct:046d:b023".overrides]
        invert_scroll = false
        "#,
    );
    let source = device("[bindings]\nBack = \"Copy\"");
    let transfer = transfer_settings(&source, &target, mouse(mx_master()));

    assert_eq!(transfer.changed, [TransferItem::Button(ButtonId::Back)]);
    assert!(transfer.settings.invert_scroll);
    assert!(transfer.settings.smooth_wheel);
    assert_eq!(
        transfer.settings.links["direct:046d:b023"]
            .overrides
            .invert_scroll,
        Some(false),
        "an inversion the source never set keeps the target's link override"
    );
}

#[test]
fn a_source_with_inversion_on_replaces_the_targets_link_override() {
    let target = device(
        r#"
        [links."direct:046d:b023".overrides]
        invert_scroll = false
        "#,
    );
    let source = device("invert_scroll = true\nsmooth_wheel = true");
    let transfer = transfer_settings(&source, &target, mouse(mx_master()));

    assert_eq!(
        transfer.changed,
        [TransferItem::InvertScroll, TransferItem::SmoothWheel]
    );
    assert!(transfer.settings.invert_scroll);
    assert_eq!(
        transfer.settings.links["direct:046d:b023"]
            .overrides
            .invert_scroll,
        None
    );
}

#[test]
fn a_wheel_reversed_in_software_takes_the_inversion() {
    let source = device("invert_scroll = true");
    // Measured, but without the firmware's invert bit.
    let capabilities = Capabilities {
        scroll_inversion: false,
        ..mx_master()
    };
    let software = TransferTarget {
        invertible_wheel: true,
        ..mouse(capabilities)
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), software);
    assert_eq!(transfer.changed, [TransferItem::InvertScroll]);
    assert!(transfer.settings.invert_scroll);

    let neither = TransferTarget {
        invertible_wheel: false,
        ..mouse(capabilities)
    };
    let transfer = transfer_settings(&source, &DeviceConfig::default(), neither);
    assert!(transfer.is_empty());
    assert_eq!(
        transfer.skipped,
        [SkippedItem {
            item: TransferItem::InvertScroll,
            reason: SkipReason::NoScrollInversion,
        }]
    );
}

/// The settings of the device `mouse` in a config that `edit` set up.
fn set_up(edit: impl FnOnce(&mut Config)) -> DeviceConfig {
    let mut config = Config::default();
    edit(&mut config);
    config.devices["mouse"].clone()
}

#[test]
fn a_copied_gesture_map_drops_the_targets_stored_directions() {
    let source = set_up(|config| {
        config.set_gesture_direction(
            "mouse",
            ButtonId::GestureButton,
            crate::binding::GestureDirection::Up,
            Action::Copy,
        );
    });
    let target = set_up(|config| {
        config.set_gesture_mode("mouse", ButtonId::GestureButton, false);
    });
    assert!(
        target
            .disabled_gestures
            .contains_key(&ButtonId::GestureButton)
    );

    let transfer = transfer_settings(&source, &target, mouse(mx_master()));

    assert_eq!(
        transfer.changed,
        [TransferItem::Button(ButtonId::GestureButton)]
    );
    assert_eq!(
        transfer.settings.bindings.get(&ButtonId::GestureButton),
        source.bindings.get(&ButtonId::GestureButton)
    );
    assert!(
        !transfer
            .settings
            .disabled_gestures
            .contains_key(&ButtonId::GestureButton),
        "a button in gesture mode stores no directions"
    );
}

#[test]
fn a_button_with_gestures_off_brings_its_stored_directions() {
    let source = set_up(|config| {
        config.set_gesture_direction(
            "mouse",
            ButtonId::GestureButton,
            crate::binding::GestureDirection::Up,
            Action::Copy,
        );
        config.set_gesture_mode("mouse", ButtonId::GestureButton, false);
    });
    let target = set_up(|config| {
        config.set_gesture_direction(
            "mouse",
            ButtonId::GestureButton,
            crate::binding::GestureDirection::Down,
            Action::Paste,
        );
    });

    let transfer = transfer_settings(&source, &target, mouse(mx_master()));

    assert_eq!(
        transfer.changed,
        [
            TransferItem::Button(ButtonId::GestureButton),
            TransferItem::StoredGestures(ButtonId::GestureButton),
        ]
    );
    assert_eq!(transfer.settings.bindings, source.bindings);
    assert_eq!(
        transfer.settings.disabled_gestures,
        source.disabled_gestures
    );
    let mut restored = Config::default();
    restored
        .devices
        .insert("mouse".to_owned(), transfer.settings);
    restored.set_gesture_mode("mouse", ButtonId::GestureButton, true);
    assert_eq!(
        restored.devices["mouse"]
            .bindings
            .get(&ButtonId::GestureButton),
        Some(&Binding::Gesture(
            source.disabled_gestures[&ButtonId::GestureButton].clone()
        )),
        "turning gestures on again restores the copied map"
    );
}

#[test]
fn stored_directions_never_join_a_button_left_in_gesture_mode() {
    // The binding stays out, so the target's gesture map stays on.
    let source = imported(
        r#"
        [bindings]
        GestureButton = { RunShellCommand = "say hi" }
        [disabled_gestures.GestureButton]
        Up = "Copy"
        "#,
    );
    let target = set_up(|config| {
        config.set_gesture_direction(
            "mouse",
            ButtonId::GestureButton,
            crate::binding::GestureDirection::Down,
            Action::Paste,
        );
    });
    let mut here = Config::default();
    here.devices.insert("mouse".to_owned(), target.clone());

    let transfer = here.settings_transfer_from_file(&source, "mouse", mouse(mx_master()));

    assert!(transfer.is_empty(), "{:?}", transfer.changed);
    assert_eq!(transfer.settings, target);
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

/// A device whose buttons, Actions Ring and app profile run commands,
/// scripts and workflows, type text and open things, next to bindings that
/// do none of that.
const PAYLOAD_ACTIONS: &str = r#"
[bindings]
Back = { RunShellCommand = "say back" }
Forward = { Workflow = [{ TypeText = "hi" }] }
MiddleClick = { TypeText = "hi" }
WheelTiltLeft = { OpenApplication = { path = "~/Downloads/run.command", display_name = "Calculator" } }
Thumbwheel = "Copy"

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
fn a_file_brings_no_action_its_payload_decides() {
    let config = Config::default();
    let transfer = config.settings_transfer_from_file(
        &imported(PAYLOAD_ACTIONS),
        "unit:new",
        mouse(mx_master()),
    );

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
        .inspect(|skipped| assert_eq!(skipped.reason, SkipReason::NotFromAFile))
        .map(|skipped| &skipped.item)
        .collect();
    for item in [
        TransferItem::Button(ButtonId::Back),
        TransferItem::Button(ButtonId::Forward),
        TransferItem::Button(ButtonId::MiddleClick),
        TransferItem::Button(ButtonId::WheelTiltLeft),
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
    assert_eq!(left_out.len(), 8);

    let settings = &transfer.settings;
    for button in [
        ButtonId::Back,
        ButtonId::Forward,
        ButtonId::MiddleClick,
        ButtonId::WheelTiltLeft,
        ButtonId::GestureButton,
    ] {
        assert!(!settings.bindings.contains_key(&button), "{button:?}");
    }
    assert!(settings.disabled_gestures.is_empty());
    assert!(settings.action_ring.is_default());
}

#[test]
fn a_device_of_this_machine_brings_every_action() {
    let transfer = transfer_settings(
        &device(PAYLOAD_ACTIONS),
        &DeviceConfig::default(),
        mouse(mx_master()),
    );

    assert!(transfer.skipped.is_empty(), "{:?}", transfer.skipped);
    assert_eq!(transfer.changed.len(), 10, "{:?}", transfer.changed);
    assert_eq!(transfer.settings.bindings, device(PAYLOAD_ACTIONS).bindings);
    assert_eq!(
        transfer.settings.action_ring,
        device(PAYLOAD_ACTIONS).action_ring
    );
}

/// A config whose old mouse has a profile and an Actions Ring layout for an
/// application named by an alias.
const ALIASED_PROFILES: &str = r#"
[apps.editor]
name = "Editor"
macos = "com.example.Editor"
linux = "editor"
windows = "exe:editor.exe"

[apps.unused]
macos = "com.example.Unused"

[devices."serial:old".per_app_bindings."app:editor"]
Back = "Copy"

[devices."serial:old".action_ring.per_app."app:editor".slots]
Top = { action = "Paste" }
"#;

#[cfg(feature = "fs")]
#[test]
fn an_export_brings_the_aliases_its_profiles_name_through_the_loader() {
    let dir = tempfile::tempdir().expect("tempdir");
    let export_path = dir.path().join("export.toml");
    config(ALIASED_PROFILES)
        .device_export("serial:old")
        .expect("the device has an entry")
        .write_copy(&export_path)
        .expect("the export is written");

    let file = Config::load_from_path(&export_path).expect("the export loads");
    let names: Vec<String> = file.apps.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(names, ["editor"], "only the aliases its profiles name");

    let mut here = Config::default();
    let source = file
        .imported_device(EXPORT_KEY)
        .expect("the export has its device");
    let transfer = here.settings_transfer_from_file(&source, "unit:new", mouse(mx_master()));
    assert_eq!(
        transfer.changed,
        [
            TransferItem::AppButton {
                app: "app:editor".to_owned(),
                button: ButtonId::Back,
            },
            TransferItem::ActionRing,
        ]
    );
    here.apply_settings_transfer("unit:new", transfer);

    let saved_path = dir.path().join("config.toml");
    here.write_copy(&saved_path).expect("the config is written");
    let saved = Config::load_from_path(&saved_path).expect("the imported profiles load");
    assert_eq!(saved.apps, file.apps);
    assert_eq!(
        saved.devices["unit:new"].per_app_bindings,
        file.devices[EXPORT_KEY].per_app_bindings
    );
}

#[test]
fn a_profile_for_an_app_defined_differently_here_stays_out() {
    let file = config(ALIASED_PROFILES);
    let source = file
        .imported_device("serial:old")
        .expect("the file has the device");
    let conflicts = [
        // The same alias for another application.
        "[apps.editor]\nmacos = \"com.example.Other\"",
        // Another alias for the same application.
        "[apps.code]\nmacos = \"com.example.Editor\"",
    ];
    for here in conflicts {
        let here = config(here);
        let transfer = here.settings_transfer_from_file(&source, "unit:new", mouse(mx_master()));

        assert!(transfer.is_empty(), "{here:?}: {:?}", transfer.changed);
        assert_eq!(
            transfer.skipped,
            [
                SkippedItem {
                    item: TransferItem::AppButton {
                        app: "app:editor".to_owned(),
                        button: ButtonId::Back,
                    },
                    reason: SkipReason::AppAliasConflict,
                },
                SkippedItem {
                    item: TransferItem::ActionRing,
                    reason: SkipReason::AppAliasConflict,
                },
            ]
        );
        assert_eq!(transfer.apps, None);
    }
}

#[test]
fn a_profile_for_an_alias_defined_the_same_here_adds_nothing_to_the_table() {
    let file = config(ALIASED_PROFILES);
    let source = file
        .imported_device("serial:old")
        .expect("the file has the device");
    let here = config(
        r#"
        [apps.editor]
        name = "Editor"
        macos = "com.example.Editor"
        linux = "editor"
        windows = "exe:editor.exe"
        "#,
    );
    let transfer = here.settings_transfer_from_file(&source, "unit:new", mouse(mx_master()));

    assert_eq!(transfer.changed.len(), 2, "{:?}", transfer.skipped);
    assert_eq!(transfer.apps, None);
}

#[cfg(feature = "fs")]
#[cfg(unix)]
#[test]
fn a_failed_export_leaves_the_earlier_file_as_it_was() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("export.toml");
    std::fs::write(&path, "earlier export").expect("the earlier export is written");
    let export = config(ALIASED_PROFILES)
        .device_export("serial:old")
        .expect("the device has an entry");
    // A folder that takes no new file: the staging file cannot be created.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
        .expect("the folder is made read-only");
    let written = export.write_copy(&path);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the folder is made writable again");

    let error = written.expect_err("a read-only folder takes no export");
    assert!(
        matches!(error, crate::config::ConfigError::Write { .. }),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the earlier export is still there"),
        "earlier export"
    );
    let entries = std::fs::read_dir(dir.path())
        .expect("the folder lists")
        .count();
    assert_eq!(entries, 1, "no partial file is left behind");
}

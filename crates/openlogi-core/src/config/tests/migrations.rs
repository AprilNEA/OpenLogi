//! Schema migrations, v1 through the current version: bindings, thumb-wheel pairs, gesture owners, direct-route keys and host-switch targets.

use super::*;

#[test]
fn migrates_v1_button_and_gesture_bindings() {
    // A pre-v2 file: split button_bindings + a flat gesture_bindings map.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
Back = \"BrowserBack\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Click = \"Paste\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    // v1 still loads (version <= current) and folds into the merged map.
    let cfg = Config::load_from_path(&path).expect("load v1");
    let bindings = cfg.stored_bindings("2b042");
    assert_eq!(
        bindings.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    let mut gesture = BTreeMap::new();
    gesture.insert(GestureDirection::Up, Action::Copy);
    gesture.insert(GestureDirection::Click, Action::Paste);
    assert_eq!(
        bindings.get(&ButtonId::GestureButton),
        Some(&Binding::Gesture(gesture))
    );

    // Saving self-heals to the current shape: stamped version + merged table,
    // legacy field names gone.
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        body.contains(&format!("schema_version = {SCHEMA_VERSION}")),
        "got: {body}"
    );
    assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
    assert!(!body.contains("button_bindings"), "got: {body}");
    assert!(!body.contains("gesture_bindings"), "got: {body}");
}

#[test]
fn migrates_pre_v7_thumbwheel_defaults_to_normalised_native_direction() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.stored_bindings("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
}

#[test]
fn migrates_a_pre_v7_per_app_pair_that_was_effectively_default() {
    // The app overrides only one half; the other old default comes from a
    // custom global pair. The migration must materialise both new defaults in
    // the app so it remains native without changing the custom global profile.
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"NextTab\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"

[devices.\"unit:6be9d300\".per_app_bindings.\"com.example.Editor\"]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let app = cfg.effective_bindings("unit:6be9d300", Some("com.example.Editor"));
    assert_eq!(
        app.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
    assert_eq!(
        app.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        cfg.stored_bindings("unit:6be9d300")
            .get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::NextTab)),
        "the custom global pair must stay literal"
    );
}

#[test]
fn pre_v7_thumbwheel_migration_leaves_a_custom_pair_unchanged() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"NextTab\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.stored_bindings("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::NextTab))
    );
}

#[test]
fn pre_v7_thumbwheel_migration_preserves_non_single_binding_shapes() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = { short = \"HorizontalScrollRight\", long = \"NextTab\" }
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.stored_bindings("unit:6be9d300");
    let Some(Binding::LongPress(up)) = bindings.get(&ButtonId::ThumbwheelScrollUp) else {
        panic!("custom long-press binding must keep its shape");
    };
    assert_eq!(up.short(), &Action::HorizontalScrollRight);
    assert_eq!(up.long(), &Action::NextTab);
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
}

#[test]
fn v7_reversed_thumbwheel_pair_survives_reload() {
    let v7 = "\
schema_version = 7

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v7).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v7");
    let bindings = cfg.stored_bindings("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
}

#[test]
fn migration_gesture_map_wins_over_legacy_single_gesture_button_entry() {
    // The data-loss guard: when a legacy single button_bindings[GestureButton]
    // entry coexists with a gesture_bindings map (reachable via hand-edited
    // or very old configs), the gesture map must survive — not be shadowed by
    // the single entry. Mirrors the pre-v2 "gesture entries win" rule.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Down = \"Paste\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v1");
    let mut gesture = BTreeMap::new();
    gesture.insert(GestureDirection::Up, Action::Copy);
    gesture.insert(GestureDirection::Down, Action::Paste);
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Gesture(gesture)),
        "gesture map must win over the legacy single GestureButton entry"
    );
}

#[test]
fn migration_drops_vestigial_lone_gesture_button_single() {
    // A v1 file with only `button_bindings[GestureButton]` and no
    // `gesture_bindings` (the pre-gesture-picker shape). That entry never
    // dispatched in v1 — the gesture button's plain press routes through the
    // gesture `Click` slot, not the per-button map — so migrating it to a
    // `Binding::Single` would leave an unreachable entry the GUI hides and the
    // runtime ignores. It must be dropped, not shadow the gesture path.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"
Back = \"BrowserBack\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    let bindings = Config::load_from_path(&path)
        .expect("load v1")
        .stored_bindings("2b042");
    // An ordinary button still migrates to a `Single`...
    assert_eq!(
        bindings.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    // ...but the vestigial gesture-button single is gone, leaving the button
    // to fall back to its canonical default rather than an unreachable entry.
    assert_eq!(bindings.get(&ButtonId::GestureButton), None);
}

#[test]
fn migration_demotes_the_dormant_non_owner_gesture_maps() {
    // An owner-locked config: Middle owns gestures; the dedicated button
    // keeps a dormant map from an earlier reign. Shape-driven load keeps the
    // owner gesturing and flattens the dormant map to its Click, so a stored
    // map's presence always MEANS gesture mode.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"MiddleClick\"

[devices.2b042.bindings]
GestureButton = { Up = \"Copy\", Click = \"Paste\" }
MiddleClick = { Up = \"MissionControl\", Click = \"MiddleClick\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::MiddleClick));
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(Action::Paste)),
        "the dormant map demotes to its Click choice"
    );
    // The legacy field is gone on save — the binding shape is the whole truth.
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("gesture_owner"), "got: {body}");
}

#[test]
fn migration_off_pins_the_dedicated_button_out_of_gesture_mode() {
    // gesture_owner = "Off" with no stored bindings: absence would re-enter
    // default gesture mode under shape rules, so the load pins the dedicated
    // button with an explicit Single at its canonical default (which the
    // capture layer treats as native/undiverted).
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"Off\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.gesture_mode_buttons("2b042").is_empty());
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(default_binding(ButtonId::GestureButton)))
    );
}

#[test]
fn migration_materializes_a_hidpp_owners_missing_map() {
    // A v3 HID++ owner dispatched the seeded default direction map
    // regardless of its stored shape (the runtime seeded at projection
    // time), so an owner with no stored map must not lose gestures when
    // the file is rewritten to the current schema.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"HapticPanel\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::HapticPanel));
    match cfg.stored_bindings("2b042").get(&ButtonId::HapticPanel) {
        Some(Binding::Gesture(map)) => {
            for dir in GestureDirection::ALL {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected the owner's materialized default map, got {other:?}"),
    }
    // The non-owner dedicated button is still pinned off.
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
}

#[test]
fn migration_replaces_a_hidpp_owners_single_with_the_default_map() {
    // A hand-edited v3 file: the owner field says GestureButton but its
    // stored binding is a Single. The v3 runtime still dispatched the full
    // default direction map (seeded at projection), so the load must
    // materialize that map rather than keep the never-dispatched Single.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"GestureButton\"

[devices.2b042.bindings]
GestureButton = \"CycleDpiPresets\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_gesture_binding(GestureDirection::Click)),
                "v3 dispatched the seeded default Click, not the stored Single"
            );
        }
        other => panic!("expected the owner's materialized default map, got {other:?}"),
    }
}

#[test]
fn migration_stashes_dormant_maps_for_re_enabling() {
    // The owner-lock model preserved every dormant non-owner map for
    // restore-on-reselection. The migration keeps that promise: the demoted
    // map is stashed, and turning the button back on restores it.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"MiddleClick\"

[devices.2b042.bindings]
GestureButton = { Up = \"Copy\", Click = \"Paste\" }
MiddleClick = { Up = \"MissionControl\", Click = \"MiddleClick\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let mut cfg = Config::load_from_path(&path).expect("load");
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));

    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Up),
                Some(&Action::Copy),
                "the dormant map's arms come back on re-enable"
            );
        }
        other => panic!("expected the stashed dormant map restored, got {other:?}"),
    }
}

#[test]
fn migration_infers_the_owner_for_pre_field_configs() {
    // Pre-owner-field file: a gesture-shaped OS-hook button was the owner by
    // inference, silencing the dedicated button. The load preserves exactly
    // that: Back keeps gesturing, the dedicated button is pinned off.
    let toml = "\
schema_version = 2

[devices.2b042.bindings]
Back = { Up = \"Copy\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::Back));
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(default_binding(ButtonId::GestureButton)))
    );
}

#[test]
fn migrating_v4_drops_the_transport_prefix_from_direct_keys() {
    let source = r#"
schema_version = 4
selected_device = "direct:046d:c08d:unit:6be9d300"

[devices."direct:046d:c08d:unit:6be9d300"]
invert_scroll = true

[devices."receiver:82839805:slot:1"]
dpi = 1600
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(config.devices.contains_key("unit:6be9d300"));
    assert!(
        !config
            .devices
            .contains_key("direct:046d:c08d:unit:6be9d300")
    );
    assert_eq!(
        config.selected_device.as_deref(),
        Some("unit:6be9d300"),
        "the selection follows the key"
    );
    assert!(
        config.devices["unit:6be9d300"]
            .links
            .contains_key("direct:046d:c08d"),
        "the route it came from is remembered, not discarded"
    );
    assert!(
        config.devices.contains_key("receiver:82839805:slot:1"),
        "receiver entries are left for runtime adoption"
    );
}

#[test]
fn migrating_two_direct_routes_of_one_device_folds_instead_of_dropping_one() {
    // An MX Master 3S reached over USB *and* over Bluetooth-direct has two v4
    // entries whose keys both rename to `unit:6be9d300`. Inserting would let
    // whichever came later in `BTreeMap` order silently delete the other's
    // bindings, DPI and lighting — the one case where phase A of the
    // migration is neither mechanical nor lossless.
    let source = r#"
schema_version = 4

[devices."direct:046d:b034:unit:6be9d300"]
dpi = 1600
invert_scroll = true

[devices."direct:046d:b034:unit:6be9d300".bindings]
Back = "BrowserBack"

[devices."direct:046d:c08d:unit:6be9d300"]
dpi = 800

[devices."direct:046d:c08d:unit:6be9d300".bindings]
Forward = "BrowserForward"
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert_eq!(config.devices.len(), 1, "one device, one entry");
    let device = &config.devices["unit:6be9d300"];
    assert_eq!(
        device.bindings.len(),
        2,
        "both routes' bindings survive: {:?}",
        device.bindings
    );
    assert_eq!(
        device.dpi,
        Some(Dpi::new(1600)),
        "one value stays canonical"
    );
    assert_eq!(
        device.links["direct:046d:c08d"].overrides.dpi,
        Some(Dpi::new(800)),
        "the other survives as an override on the route it was set for"
    );
    assert!(
        device.links.contains_key("direct:046d:b034"),
        "both routes are indexed: {:?}",
        device.links
    );
}

#[test]
fn loading_hand_edited_duplicate_routes_keeps_the_first_device_key() {
    let source = r#"
schema_version = 6
selected_device = "unit:ffffffff"

[devices.keyboard]
host_switch_targets = ["unit:ffffffff"]

[devices."unit:11111111".links."receiver:82839805:slot:1".overrides]
dpi = 800

[devices."unit:ffffffff".links."receiver:82839805:slot:1".overrides]
dpi = 1600
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write hand-edited config");

    let config = Config::load_from_path(&path).expect("load");

    assert_eq!(
        config.devices["unit:11111111"].links["receiver:82839805:slot:1"]
            .overrides
            .dpi,
        Some(Dpi::new(800)),
        "the lexicographically first device key keeps its complete link"
    );
    assert!(
        !config.devices["unit:ffffffff"]
            .links
            .contains_key("receiver:82839805:slot:1"),
        "the later duplicate no longer indexes the route"
    );
    assert_eq!(config.selected_device.as_deref(), Some("unit:ffffffff"));
    assert_eq!(
        config.devices["keyboard"].host_switch_targets,
        vec!["unit:ffffffff".to_string()],
        "repairing an index does not rename the referenced device entry"
    );
}

#[test]
fn loading_v4_repairs_duplicate_routes_created_by_key_migration() {
    let source = r#"
schema_version = 4
selected_device = "direct:046d:c08d:unit:11111111"

[devices.keyboard]
host_switch_targets = ["direct:046d:c08d:unit:11111111"]

[devices."direct:046d:c08d:unit:11111111"]
dpi = 1600

[devices."unit:ffffffff".links."direct:046d:c08d".overrides]
dpi = 800
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write v4 config");

    let config = Config::load_from_path(&path).expect("load and migrate");

    assert!(
        config.devices["unit:11111111"]
            .links
            .contains_key("direct:046d:c08d"),
        "the first post-migration device key owns the route"
    );
    assert!(
        !config.devices["unit:ffffffff"]
            .links
            .contains_key("direct:046d:c08d"),
        "the pre-existing duplicate is removed"
    );
    assert_eq!(
        config.selected_device.as_deref(),
        Some("unit:11111111"),
        "selection follows the migrated device key"
    );
    assert_eq!(
        config.devices["keyboard"].host_switch_targets,
        vec!["unit:11111111".to_string()],
        "host-switch references follow the migrated device key"
    );
}

#[test]
fn migrating_rewrites_host_switch_targets() {
    let source = r#"
schema_version = 4

[devices."receiver:82839805:slot:2"]
host_switch_targets = ["direct:046d:c08d:unit:6be9d300"]
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();
    assert_eq!(
        config.devices["receiver:82839805:slot:2"].host_switch_targets,
        vec!["unit:6be9d300".to_string()],
    );
}

#[test]
fn a_v4_file_with_no_direct_entries_is_untouched() {
    let source = "schema_version = 4\n\n[devices.\"receiver:82839805:slot:1\"]\ndpi = 1600\n";
    let mut config: Config = toml::from_str(source).expect("parses");
    let before = config.devices.clone();
    config.migrate_transport_scoped_keys();
    assert_eq!(config.devices.len(), before.len());
    assert!(config.devices.contains_key("receiver:82839805:slot:1"));
}

#[test]
fn migrating_v4_drops_the_transport_prefix_from_serial_keyed_direct_keys() {
    // The identity fragment is not always `unit:<hex>` — a device that
    // reports a serial keys as `serial:<s>` instead, and the brief names
    // both halves of the mapping equally.
    let source = r#"
schema_version = 4

[devices."direct:046d:c08d:serial:abc123"]
invert_scroll = true
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(config.devices.contains_key("serial:abc123"));
    assert!(
        !config
            .devices
            .contains_key("direct:046d:c08d:serial:abc123")
    );
    assert!(
        config.devices["serial:abc123"]
            .links
            .contains_key("direct:046d:c08d"),
        "the route it came from is remembered, not discarded"
    );
}

#[test]
fn migrating_leaves_a_direct_key_with_no_physical_identity_untouched() {
    // An all-zero unit id is not a physical identity (see
    // `PhysicalDeviceKey::parse`), so this key must not be rewritten — doing
    // so would collide every never-identified direct device onto one
    // `unit:00000000` entry.
    let source = "schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:00000000\"]\ninvert_scroll = true\n";
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(
        config
            .devices
            .contains_key("direct:046d:c08d:unit:00000000"),
        "a non-physical identity is left keyed exactly as it was loaded"
    );
    assert!(!config.devices.contains_key("unit:00000000"));
}

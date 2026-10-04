use serde_json::json;

use super::*;
use crate::binding::{Action, GestureDirection};

const SOURCE: &str = "mx-master-3s-synthetic-001";
const TARGET: &str = "unit:00000001";

fn document() -> Value {
    json!({"schema_version":26, "profile_keys":["profile-base", "profile-app"],
        "profile-base": {"id":"base-id", "name":"Global", "applicationId":"desktop", "activeForApplication":true,
            "assignments":[{"slotId":format!("{SOURCE}_c83"), "card":shortcut(6, &[227])}]},
        "profile-app": {"id":"app-id", "applicationId":"editor", "baseProfileId":"base-id",
            "assignments":[{"slotId":format!("{SOURCE}_c83"), "card":shortcut(6, &[227])}]},
        "applications":{"applications":[{"applicationId":"editor", "applicationPath":"/Applications/Example.app"}]}
    })
}

fn shortcut(code: u8, modifiers: &[u8]) -> Value {
    json!({"attribute":"MACRO_PLAYBACK", "macro":{"type":"KEYSTROKE", "actionName":"untrusted label",
        "keystroke":{"code":code,"modifiers":modifiers}}})
}

fn current() -> Config {
    let mut config = Config::default();
    let device = config.devices.entry(TARGET.into()).or_default();
    device
        .bindings
        .insert(ButtonId::Forward, Binding::Single(Action::Paste));
    device
        .per_app_bindings
        .entry("com.example.Other".into())
        .or_default()
        .insert(ButtonId::Back, Action::Undo);
    config
}

fn merge(document: &Value) -> ImportResult {
    OptionsSettings::parse(&serde_json::to_vec(document).unwrap())
        .unwrap()
        .merge(&current(), SOURCE, TARGET, |_| {
            Some("com.example.Editor".into())
        })
        .unwrap()
}

#[test]
fn equal_app_assignments_survive_later_global_changes() {
    let result = merge(&document());
    assert_eq!(result.imported(), 2);
    let mut config = result.config().clone();
    config
        .devices
        .get_mut(TARGET)
        .unwrap()
        .bindings
        .insert(ButtonId::Back, Binding::Single(Action::Undo));
    assert_eq!(
        config.effective_bindings(TARGET, Some("com.example.Editor"))[&ButtonId::Back],
        Binding::Single(Action::CustomShortcut("Super+C".parse().unwrap()))
    );
    assert_eq!(
        config.devices[TARGET].bindings[&ButtonId::Forward],
        Binding::Single(Action::Paste)
    );
    assert_eq!(
        config.devices[TARGET].per_app_bindings["com.example.Other"][&ButtonId::Back],
        Action::Undo
    );
}

#[test]
fn inactive_app_profile_preserves_existing_overrides_and_reports_notice() {
    let mut source = document();
    source["profile-app"]["activeForApplication"] = json!(false);
    let mut config = current();
    config
        .devices
        .get_mut(TARGET)
        .unwrap()
        .per_app_bindings
        .entry("com.example.Editor".into())
        .or_default()
        .insert(ButtonId::Back, Action::Undo);
    let settings = OptionsSettings::parse(&serde_json::to_vec(&source).unwrap()).unwrap();
    let result = settings
        .merge(&config, SOURCE, TARGET, |_| {
            panic!("inactive profiles must not resolve or import their application")
        })
        .unwrap();
    assert_eq!(result.imported(), 1);
    assert_eq!(
        result.config().devices[TARGET].bindings[&ButtonId::Back],
        Binding::Single(Action::CustomShortcut("Super+C".parse().unwrap()))
    );
    assert_eq!(
        result.config().devices[TARGET].per_app_bindings,
        config.devices[TARGET].per_app_bindings
    );
    assert_eq!(result.notices().len(), 1);
    assert_eq!(result.notices()[0].kind(), NoticeKind::InactiveProfile);
    assert_eq!(result.notices()[0].profile(), "editor");
}

#[test]
fn inactive_base_profile_is_rejected() {
    let mut source = document();
    source["profile-base"]["activeForApplication"] = json!(false);
    assert!(matches!(
        OptionsSettings::parse(&serde_json::to_vec(&source).unwrap()),
        Err(ImportError::Invalid(_))
    ));
}

#[test]
fn duplicate_application_catalog_ids_are_rejected() {
    let mut source = document();
    source["applications"]["applications"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "applicationId":"editor",
            "applicationPath":"/Applications/Other.app"
        }));
    assert!(matches!(
        OptionsSettings::parse(&serde_json::to_vec(&source).unwrap()),
        Err(ImportError::Invalid(_))
    ));
}

#[test]
fn profiles_resolving_to_the_same_bundle_are_rejected() {
    let mut source = document();
    source["profile_keys"]
        .as_array_mut()
        .unwrap()
        .push(json!("profile-other"));
    source["profile-other"] = source["profile-app"].clone();
    source["profile-other"]["id"] = json!("other-id");
    source["profile-other"]["applicationId"] = json!("other");
    source["applications"]["applications"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "applicationId":"other",
            "applicationPath":"/Applications/Other.app"
        }));
    let settings = OptionsSettings::parse(&serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(matches!(
        settings.merge(&current(), SOURCE, TARGET, |_| {
            Some("com.example.Editor".into())
        }),
        Err(ImportError::Invalid(_))
    ));
}

#[test]
fn keyboard_payload_is_authoritative_and_invalid_keys_are_skipped() {
    let mut source = document();
    source["profile-base"]["assignments"][0]["card"] = shortcut(43, &[224, 225]);
    let result = merge(&source);
    assert_eq!(
        result.config().devices[TARGET].bindings[&ButtonId::Back],
        Binding::Single(Action::CustomShortcut("Ctrl+Shift+Tab".parse().unwrap()))
    );
    for card in [
        shortcut(0, &[227]),
        shortcut(6, &[231]),
        shortcut(255, &[]),
        json!({"attribute":"MACRO_PLAYBACK","macro":{}}),
    ] {
        source["profile-base"]["assignments"][0]["card"] = card;
        let result = merge(&source);
        assert!(
            !result.config().devices[TARGET]
                .bindings
                .contains_key(&ButtonId::Back)
        );
        assert!(
            result
                .notices()
                .iter()
                .any(|notice| notice.kind() == NoticeKind::UnsupportedAction)
        );
    }
}

#[test]
fn selected_gesture_is_atomic_and_app_gestures_are_not_flattened() {
    let mut source = document();
    let cards: serde_json::Map<_, _> = ["up", "down", "left", "right", "click"]
        .into_iter()
        .map(|direction| (direction.into(), shortcut(6, &[227])))
        .collect();
    let gesture = json!({"attribute":"ONE_OF", "selectedNestedCard":"custom",
        "nestedCards":{"unused":{},"custom":{"attribute":"ADAPTER_4WAYS", "nestedCards":cards}}});
    for key in ["profile-base", "profile-app"] {
        source[key]["assignments"][0]["slotId"] = format!("{SOURCE}_c195").into();
        source[key]["assignments"][0]["card"] = gesture.clone();
    }
    let result = merge(&source);
    assert_eq!(result.imported(), 1);
    let Binding::Gesture(gestures) =
        &result.config().devices[TARGET].bindings[&ButtonId::GestureButton]
    else {
        panic!("gesture lost")
    };
    assert_eq!(gestures.len(), GestureDirection::ALL.len());
    assert!(
        result
            .notices()
            .iter()
            .any(|n| n.kind() == NoticeKind::AppGesture)
    );
    source["profile-base"]["assignments"][0]["card"]["nestedCards"]["custom"]["nestedCards"]["left"] =
        json!({});
    let result = merge(&source);
    assert!(
        !result.config().devices[TARGET]
            .bindings
            .contains_key(&ButtonId::GestureButton)
    );
}

#[test]
fn unsupported_settings_ring_and_wheel_are_reported_without_erasing_target() {
    let mut source = document();
    let assignments = source["profile-base"]["assignments"]
        .as_array_mut()
        .unwrap();
    for slot in [
        format!("{SOURCE}_mouse_settings"),
        format!("{SOURCE}_thumb_wheel_adapter"),
        "radial-menu-virtual-device-synthetic_c409".into(),
    ] {
        assignments.push(json!({"slotId":slot, "card":{}}));
    }
    let result = merge(&source);
    for kind in [
        NoticeKind::DeviceSetting,
        NoticeKind::Wheel,
        NoticeKind::VirtualDevice,
    ] {
        assert!(result.notices().iter().any(|notice| notice.kind() == kind));
    }
    let missing_app = OptionsSettings::parse(&serde_json::to_vec(&source).unwrap())
        .unwrap()
        .merge(&current(), SOURCE, TARGET, |_| None)
        .unwrap();
    assert_eq!(missing_app.imported(), 1);
    assert!(
        missing_app
            .notices()
            .iter()
            .any(|n| n.kind() == NoticeKind::Application)
    );
}

#[test]
fn malformed_and_ambiguous_sources_fail_closed() {
    let mut cases = Vec::new();
    let mut source = document();
    source["schema_version"] = json!(27);
    cases.push(source);
    let mut source = document();
    source["profile-app"]["baseProfileId"] = json!("profile-base");
    cases.push(source);
    let mut source = document();
    source["profile-app"]["id"] = json!("base-id");
    cases.push(source);
    let mut source = document();
    source["profile_keys"] = json!(["missing"]);
    cases.push(source);
    let mut source = document();
    source["profile-base"]["assignments"]
        .as_array_mut()
        .unwrap()
        .push(document()["profile-base"]["assignments"][0].clone());
    cases.push(source);
    for source in cases {
        assert!(
            OptionsSettings::parse(&serde_json::to_vec(&source).unwrap()).is_err(),
            "malformed source accepted"
        );
    }
    assert!(
        OptionsSettings::parse(&vec![b' '; MAX_DOCUMENT_BYTES + 1]).is_err(),
        "unbounded document accepted"
    );
    let settings = OptionsSettings::parse(&serde_json::to_vec(&document()).unwrap()).unwrap();
    assert!(
        settings
            .merge(&current(), SOURCE, "wrong-target", |_| None)
            .is_err(),
        "guessed target"
    );
}

#[test]
fn observed_action_payloads_map_to_their_intended_actions() {
    let cases = [
        (json!({"type":"DO_NOTHING","doNothing":{}}), Action::None),
        (
            json!({"type":"MOUSE","mouse":{"action":"BUTTON","hidUsage":1}}),
            Action::LeftClick,
        ),
        (
            json!({"type":"MOUSE","mouse":{"action":"BUTTON","hidUsage":2}}),
            Action::RightClick,
        ),
        (
            json!({"type":"MOUSE","mouse":{"action":"BUTTON","hidUsage":3}}),
            Action::MiddleClick,
        ),
        (
            json!({"type":"MOUSE","mouse":{"action":"OSX_GESTURE_BACK"}}),
            Action::BrowserBack,
        ),
        (
            json!({"type":"MOUSE","mouse":{"action":"OSX_GESTURE_FORWARD"}}),
            Action::BrowserForward,
        ),
        (
            json!({"type":"QUICK_LAUNCH","quickLaunch":{"action":"MISSION_CONTROL"}}),
            Action::MissionControl,
        ),
        (
            json!({"type":"QUICK_LAUNCH","quickLaunch":{"action":"APP_EXPOSE"}}),
            Action::AppExpose,
        ),
        (
            json!({"type":"QUICK_LAUNCH","quickLaunch":{"action":"LAUNCHPAD"}}),
            Action::LaunchpadShow,
        ),
        (
            json!({"type":"SYSTEM","system":{"action":"SWITCH_BETWEEN_DESKTOPS_LEFT"}}),
            Action::PreviousDesktop,
        ),
        (
            json!({"type":"SYSTEM","system":{"action":"SWITCH_BETWEEN_DESKTOPS_RIGHT"}}),
            Action::NextDesktop,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"PLAY_PAUSE"}}),
            Action::PlayPause,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"NEXT_TRACK"}}),
            Action::NextTrack,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"PREVIOUS_TRACK"}}),
            Action::PrevTrack,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"VOLUME_UP"}}),
            Action::VolumeUp,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"VOLUME_DOWN"}}),
            Action::VolumeDown,
        ),
        (
            json!({"type":"MEDIA","media":{"usage":"MUTE"}}),
            Action::MuteVolume,
        ),
    ];
    for (payload, expected) in cases {
        let mut source = document();
        source["profile-base"]["assignments"][0]["card"] =
            json!({"attribute":"MACRO_PLAYBACK","macro":payload});
        let result = merge(&source);
        assert_eq!(
            result.config().devices[TARGET].bindings[&ButtonId::Back],
            Binding::Single(expected.clone())
        );
        assert_eq!(
            result
                .notices()
                .iter()
                .any(|n| n.kind() == NoticeKind::Navigation),
            matches!(expected, Action::BrowserBack | Action::BrowserForward)
        );
    }
}

#[test]
fn mode_shift_requires_the_exact_observed_preset() {
    let mut source = document();
    let mode = json!({"attribute":"MACRO_PLAYBACK","id":"card_global_presets_mode_shift","taskId":157,"macro":{}});
    source["profile-base"]["assignments"][0]["card"] = mode.clone();
    assert_eq!(
        merge(&source).config().devices[TARGET].bindings[&ButtonId::Back],
        Binding::Single(Action::ToggleSmartShift)
    );
    for (key, value) in [
        ("id", json!("unknown")),
        ("taskId", json!(158)),
        ("macro", json!({"unexpected":true})),
    ] {
        let mut invalid = mode.clone();
        invalid[key] = value;
        source["profile-base"]["assignments"][0]["card"] = invalid;
        assert!(
            !merge(&source).config().devices[TARGET]
                .bindings
                .contains_key(&ButtonId::Back)
        );
    }
}

#[cfg(feature = "fs")]
#[cfg(target_os = "macos")]
mod database {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn committed_wal_is_read_and_source_rows_are_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.db");
        let mut writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .execute_batch("CREATE TABLE data(file BLOB NOT NULL)")
            .unwrap();
        let first = serde_json::to_vec(&document()).unwrap();
        writer
            .execute("INSERT INTO data VALUES (?)", [&first])
            .unwrap();
        let changed = serde_json::to_vec(&json!({"schema_version":27})).unwrap();
        {
            let transaction = writer.transaction().unwrap();
            transaction
                .execute("UPDATE data SET file = ?", [&changed])
                .unwrap();
            assert_eq!(
                read_database(&path).unwrap(),
                first,
                "uncommitted change leaked"
            );
            transaction.commit().unwrap();
        }
        assert_eq!(
            read_database(&path).unwrap(),
            changed,
            "committed WAL was missed"
        );
        assert_eq!(
            writer
                .query_row("SELECT file FROM data", [], |row| row.get::<_, Vec<u8>>(0))
                .unwrap(),
            changed
        );
        writer
            .execute("INSERT INTO data VALUES (?)", [&first])
            .unwrap();
        assert!(read_database(&path).is_err(), "ambiguous rows accepted");
    }

    #[test]
    fn missing_corrupt_locked_and_oversized_databases_are_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.db");
        assert!(read_database(&path).is_err(), "missing database accepted");
        assert!(!path.exists(), "read created a database");
        std::fs::write(&path, b"invalid").unwrap();
        assert!(read_database(&path).is_err(), "invalid database accepted");
        std::fs::remove_file(&path).unwrap();
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("CREATE TABLE data(file BLOB NOT NULL); INSERT INTO data VALUES (zeroblob(9000000));").unwrap();
        assert!(read_database(&path).is_err(), "oversized data accepted");
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(read_database(&path).is_err(), "locked database accepted");
    }

    #[test]
    fn recursive_view_cannot_turn_a_settings_read_into_an_unbounded_query() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("CREATE VIEW data AS WITH RECURSIVE counter(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM counter) SELECT zeroblob(1) AS file FROM counter;").unwrap();
        assert!(
            matches!(read_database(&path), Err(ImportError::Invalid(_))),
            "accepted a view instead of the settings table"
        );
    }
}

#[cfg(feature = "fs")]
#[cfg(target_os = "macos")]
mod recovery {
    use super::*;
    use crate::config::{ConfigError, RecoveryPlan, recovery_backups};
    use rusqlite::Connection;

    fn database(path: &std::path::Path) -> Connection {
        let writer = Connection::open(path).unwrap();
        writer
            .execute_batch("CREATE TABLE data(file BLOB NOT NULL)")
            .unwrap();
        writer
            .execute(
                "INSERT INTO data VALUES (?)",
                [serde_json::to_vec(&document()).unwrap()],
            )
            .unwrap();
        writer
    }

    #[test]
    fn options_confirmation_rejects_fifo_source_and_releases_writer_lock() {
        use std::{fs, sync::mpsc, thread, time::Duration};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("settings.db");
        let target = dir.path().join("config.toml");
        let writer = database(&source);
        current().save_to_path(&target).unwrap();
        let original = fs::read(&target).unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).unwrap();
        drop(writer);
        fs::remove_file(&source).unwrap();
        crate::file_input::tests::create_fifo(&source);
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || tx.send(plan.apply()).unwrap());
        let answer = rx.recv_timeout(Duration::from_secs(1));
        if matches!(answer, Err(mpsc::RecvTimeoutError::Timeout)) {
            drop(
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&source)
                    .unwrap(),
            );
        }
        reader.join().unwrap();
        assert!(matches!(
            answer.expect("Options+ confirmation blocked on a FIFO source"),
            Err(ConfigError::Import(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), original);
        let (config, mut file) = crate::config::ConfigFile::load_from_path(&target).unwrap();
        file.save(&config)
            .expect("a rejected Options+ FIFO released the writer lock");
    }

    #[test]
    fn options_preview_rejects_special_oversized_and_symlink_targets() {
        use std::{fs, sync::mpsc, thread, time::Duration};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("settings.db");
        let target = dir.path().join("config.toml");
        let referent = dir.path().join("managed-config.toml");
        let _writer = database(&source);
        current().save_to_path(&referent).unwrap();
        let original = fs::read(&referent).unwrap();
        std::os::unix::fs::symlink(&referent, &target).unwrap();
        assert!(matches!(
            RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET),
            Err(ConfigError::Read { path, .. }) if path == target
        ));
        assert_eq!(fs::read_link(&target).unwrap(), referent);
        assert_eq!(fs::read(&referent).unwrap(), original);
        fs::remove_file(&target).unwrap();
        fs::write(&target, &original).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        assert!(matches!(
            RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET),
            Err(ConfigError::Read { path, source })
                if path == target && source.kind() == std::io::ErrorKind::InvalidData
        ));
        fs::remove_file(&target).unwrap();
        crate::file_input::tests::create_fifo(&target);
        let (tx, rx) = mpsc::channel();
        let read_target = target.clone();
        let read_source = source.clone();
        let reader = thread::spawn(move || {
            tx.send(RecoveryPlan::prepare_options(
                &read_target,
                &read_source,
                SOURCE,
                TARGET,
            ))
            .unwrap();
        });
        let answer = rx.recv_timeout(Duration::from_secs(1));
        if matches!(answer, Err(mpsc::RecvTimeoutError::Timeout)) {
            drop(
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&target)
                    .unwrap(),
            );
        }
        reader.join().unwrap();
        assert!(matches!(
            answer.expect("Options+ preview blocked on a FIFO target"),
            Err(ConfigError::Read { path, .. }) if path == target
        ));
        assert_eq!(fs::read(&referent).unwrap(), original);
    }

    #[test]
    fn options_preview_and_apply_preserve_source_other_settings_and_recovery_point() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("settings.db");
        let _writer = database(&source);
        current().save_to_path(&target).unwrap();
        let original = std::fs::read(&target).unwrap();
        let source_bytes = read_database(&source).unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).unwrap();
        assert!(plan.is_options_import());
        assert!(!plan.changes().is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), original);
        plan.apply().unwrap();
        let loaded = Config::load_from_path(&target).unwrap();
        assert_eq!(
            loaded.devices[TARGET].bindings[&ButtonId::Forward],
            Binding::Single(Action::Paste)
        );
        assert!(
            loaded.devices[TARGET]
                .per_app_bindings
                .contains_key("com.example.Other")
        );
        assert_eq!(
            std::fs::read(recovery_backups(&target).unwrap().first().unwrap()).unwrap(),
            original
        );
        assert_eq!(read_database(&source).unwrap(), source_bytes);
    }

    #[test]
    fn source_and_target_changes_reject_an_options_plan() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("settings.db");
        let writer = database(&source);
        current().save_to_path(&target).unwrap();
        let original = std::fs::read(&target).unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).unwrap();
        writer
            .execute("UPDATE data SET file = ?", [b"{}".as_slice()])
            .unwrap();
        assert!(matches!(plan.apply(), Err(ConfigError::Conflict { .. })));
        assert_eq!(std::fs::read(&target).unwrap(), original);
        writer
            .execute(
                "UPDATE data SET file = ?",
                [serde_json::to_vec(&document()).unwrap()],
            )
            .unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).unwrap();
        std::fs::write(&target, b"external edit").unwrap();
        assert!(matches!(plan.apply(), Err(ConfigError::Conflict { .. })));
        assert_eq!(std::fs::read(&target).unwrap(), b"external edit");
        assert!(
            RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).is_err(),
            "invalid target accepted"
        );
    }

    #[test]
    fn application_identity_changed_after_preview_rejects_import() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Example.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let info = bundle.join("Contents/Info.plist");
        let write_identity = |id: &str| {
            let mut values = plist::Dictionary::new();
            values.insert("CFBundleIdentifier".into(), plist::Value::String(id.into()));
            plist::Value::Dictionary(values).to_file_xml(&info).unwrap();
        };
        write_identity("com.example.Editor");
        let target = dir.path().join("config.toml");
        let source = dir.path().join("settings.db");
        let writer = database(&source);
        let mut data = document();
        data["applications"]["applications"][0]["applicationPath"] =
            bundle.to_str().unwrap().into();
        writer
            .execute(
                "UPDATE data SET file = ?",
                [serde_json::to_vec(&data).unwrap()],
            )
            .unwrap();
        current().save_to_path(&target).unwrap();
        let before = std::fs::read(&target).unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, SOURCE, TARGET).unwrap();
        write_identity("com.example.Other");
        assert!(matches!(plan.apply(), Err(ConfigError::Conflict { .. })));
        assert_eq!(std::fs::read(&target).unwrap(), before);
    }

    #[test]
    #[ignore = "requires a local user-authorized Options+ database"]
    fn optionsplus_local_sample_imports_into_temporary_config_only() {
        let source = std::path::PathBuf::from(
            std::env::var_os("OPENLOGI_OPTIONS_SAMPLE_DB").expect("set OPENLOGI_OPTIONS_SAMPLE_DB"),
        );
        let before = read_database(&source).unwrap();
        let settings = OptionsSettings::parse(&before).unwrap();
        let device = settings
            .devices()
            .next()
            .expect("sample has no supported mouse");
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        current().save_to_path(&target).unwrap();
        let plan = RecoveryPlan::prepare_options(&target, &source, device, TARGET).unwrap();
        println!(
            "Actual sample: {} preview changes, {} notices",
            plan.changes().len(),
            plan.notices().len()
        );
        assert!(!plan.changes().is_empty());
        plan.apply().unwrap();
        let config = Config::load_from_path(&target).unwrap();
        assert!(!config.devices[TARGET].bindings.is_empty());
        assert_eq!(read_database(&source).unwrap(), before);
    }
}

#[test]
fn import_rejects_targets_without_mouse_button_capabilities() {
    let mut device = crate::config::DeviceConfig::default();
    assert!(
        can_import_into(&device),
        "older identities require an explicit user match"
    );
    device.identity = Some(crate::config::DeviceIdentity {
        display_name: "Other mouse".into(),
        model_info: None,
        codename: None,
        kind: crate::device::DeviceKind::Mouse,
        capabilities: crate::device::Capabilities::default(),
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    });
    assert!(!can_import_into(&device));
    device.identity.as_mut().unwrap().capabilities.buttons = true;
    assert!(can_import_into(&device));
    device.identity.as_mut().unwrap().kind = crate::device::DeviceKind::Keyboard;
    assert!(!can_import_into(&device));
}

use std::fs;

use openlogi_core::{
    color::Rgb,
    config::{Config, Lighting},
    device::{Capabilities, DeviceModelInfo, DeviceTransports},
    paths::CONFIG_FILE,
};

use super::*;
use crate::cmd::api::lighting::{LightingArgs, Power};

fn keyboard() -> AgentSnapshot {
    let mut snapshot = snapshot();
    let paired = &mut snapshot.inventory[0].paired[0];
    paired.kind = DeviceKind::Keyboard;
    paired.capabilities = Some(Capabilities {
        lighting: true,
        ..Capabilities::default()
    });
    paired.model_info = Some(DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [0x12, 0x34, 0x56, 0x78],
        transports: DeviceTransports::default(),
        model_ids: [0xc33c, 0, 0],
        extended_model_id: 0,
    });
    snapshot
}

fn args() -> LightingArgs {
    LightingArgs {
        device: route().to_string(),
        status: false,
        set: Some(Power::Toggle),
        color: None,
        brightness: None,
    }
}

fn original() -> Lighting {
    Lighting {
        enabled: true,
        color: Rgb::new(0x12, 0x34, 0x56),
        brightness: 37,
    }
}

fn config(path: &Path) {
    let mut config = Config::default();
    config.set_lighting("unit:12345678", original());
    config.set_dpi("unit:87654321", 2600.into());
    config.save_to_path(path).unwrap();
    let text = fs::read_to_string(path).unwrap();
    fs::write(path, format!("# keep this comment\n{text}")).unwrap();
}

fn reload(path: &Path, expected: Lighting) -> Step {
    let path = path.to_owned();
    Box::new(move |request| {
        assert!(matches!(request, AgentRequest::ReloadConfig {}));
        let saved = Config::load_from_path(&path).unwrap();
        assert_eq!(saved.lighting("unit:12345678"), Some(expected));
        assert_eq!(saved.dpi("unit:87654321"), Some(2600.into()));
        AgentResponse::ReloadConfig(Ok(()))
    })
}

fn write(expected: Lighting) -> Step {
    Box::new(move |request| {
        let AgentRequest::SetLighting {
            route: requested,
            lighting,
        } = request
        else {
            panic!("unexpected RPC {request:?}");
        };
        assert_eq!(requested, route());
        assert_eq!(lighting, expected);
        AgentResponse::SetLighting(Ok(()))
    })
}

#[tokio::test]
async fn lighting_toggle_saves_before_hardware_and_preserves_colour_brightness_and_other_devices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    config(&path);
    let mut off = original();
    off.enabled = false;
    for expected in [off, original()] {
        let result = run_script_at(
            keyboard(),
            ApiCommand::Lighting(args()),
            vec![reload(&path, expected.clone()), write(expected.clone())],
            Some(&path),
        )
        .await;
        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["source"], "configuration");
        assert_eq!(result["data"]["persistence"], "saved");
        assert_eq!(result["data"]["enabled"], expected.enabled);
        assert_eq!(result["data"]["color"], "123456");
        assert_eq!(result["data"]["brightness"], 37);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("# keep this comment")
        );
    }
}

#[tokio::test]
async fn lighting_status_reports_saved_values_without_any_hardware_or_file_write() {
    use crate::cmd::api::lighting::{Persistence, run_at};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    config(&path);
    let before = fs::read_to_string(&path).unwrap();
    let client = in_memory_agent(
        |request| panic!("status must not send an RPC: {request:?}"),
        pending(),
    );
    let mut status = args();
    status.set = None;
    for explicit in [false, true] {
        status.status = explicit;
        let result = run_at(&client, &keyboard(), &status, &path, Persistence::Read)
            .await
            .unwrap();
        assert_eq!(result["source"], "configuration");
        assert_eq!(result["enabled"], true);
        assert_eq!(result["color"], "123456");
        assert_eq!(result["brightness"], 37);
        assert_eq!(result["configured"], true);
        assert_eq!(result["persistence"], "not_saved");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }
    let missing = dir.path().join("missing.toml");
    let result = run_at(&client, &keyboard(), &status, &missing, Persistence::Read)
        .await
        .unwrap();
    assert_eq!(result["configured"], false);
    assert!(!missing.exists());
}

#[tokio::test]
async fn lighting_reload_failure_reports_saved_state_and_never_sends_hardware_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    config(&path);
    let result = run_script_at(
        keyboard(),
        ApiCommand::Lighting(args()),
        vec![Box::new(|request| {
            assert!(matches!(request, AgentRequest::ReloadConfig {}));
            AgentResponse::ReloadConfig(Err(openlogi_ipc::ConfigReloadError {
                message: "rejected".into(),
            }))
        })],
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "config_reload_failed");
    assert_eq!(result["error"]["persistence"], "saved");
}

#[tokio::test]
async fn lighting_partial_colour_and_brightness_change_does_not_turn_disabled_lights_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let mut settings = Config::default();
    let mut off = original();
    off.enabled = false;
    settings.set_lighting("unit:12345678", off);
    settings.set_dpi("unit:87654321", 2600.into());
    settings.save_to_path(&path).unwrap();
    let mut change = args();
    change.set = None;
    change.color = Some(Rgb::WHITE);
    change.brightness = Some(0);
    let expected = Lighting {
        enabled: false,
        color: Rgb::WHITE,
        brightness: 0,
    };
    let result = run_script_at(
        keyboard(),
        ApiCommand::Lighting(change),
        vec![reload(&path, expected.clone()), write(expected)],
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["enabled"], false);
}

#[tokio::test]
async fn lighting_changes_without_save_and_save_without_changes_make_no_rpc() {
    let result = run_script(keyboard(), ApiCommand::Lighting(args()), vec![]).await;
    assert_eq!(result["error"]["code"], "invalid_value");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let mut read = args();
    read.set = None;
    let result = run_script_at(keyboard(), ApiCommand::Lighting(read), vec![], Some(&path)).await;
    assert_eq!(result["error"]["code"], "invalid_value");
    assert!(!path.exists());
}

#[tokio::test]
async fn lighting_unsupported_device_does_not_save_or_send_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let result = run_script_at(
        snapshot(),
        ApiCommand::Lighting(args()),
        vec![],
        Some(&path),
    )
    .await;
    assert_eq!(result["error"]["code"], "unsupported_feature");
    assert!(!path.exists());
}

#[tokio::test]
async fn lighting_hardware_error_reports_already_saved_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    config(&path);
    let mut off = original();
    off.enabled = false;
    let result = run_script_at(
        keyboard(),
        ApiCommand::Lighting(args()),
        vec![
            reload(&path, off.clone()),
            Box::new(|request| {
                assert!(matches!(request, AgentRequest::SetLighting { .. }));
                AgentResponse::SetLighting(Err(WriteError::Hid(
                    "simulated transport failure".into(),
                )))
            }),
        ],
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["persistence"], "saved");
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .lighting("unit:12345678"),
        Some(off)
    );
}

#[test]
fn lighting_cli_rejects_invalid_colours_brightness_and_switch_values() {
    for options in [
        vec!["--colour", "red"],
        vec!["--colour", "#ffffff"],
        vec!["--colour", "+00000"],
        vec!["--brightness", "101"],
        vec!["--brightness", "-1"],
        vec!["--set", "maybe"],
        vec!["--status", "--set", "toggle"],
        vec!["--status", "--colour", "ffffff"],
        vec!["--status", "--brightness", "50"],
    ] {
        let mut command = vec!["openlogi", "api", "lighting", "--device", "test"];
        command.extend(options);
        crate::Cli::try_parse_from(command)
            .expect_err("invalid lighting arguments must be rejected");
    }
}

#[test]
fn lighting_cli_uses_set_and_accepts_both_colour_spellings() {
    for spelling in ["--colour", "--color"] {
        let parsed = crate::Cli::try_parse_from([
            "openlogi",
            "api",
            "lighting",
            "--device",
            "test",
            "--set",
            "on",
            spelling,
            "ff0000",
            "--brightness",
            "60",
            "--save",
        ])
        .unwrap();
        let Some(crate::cmd::Command::Api(ApiArgs {
            command: ApiCommand::Lighting(args),
            save: true,
        })) = parsed.cmd
        else {
            panic!("expected a saved lighting command")
        };
        assert!(matches!(args.set, Some(Power::On)));
        assert_eq!(args.color, Some(Rgb::new(255, 0, 0)));
        assert_eq!(args.brightness, Some(60));
    }
}

#[test]
fn lighting_invalid_operations_are_validated_before_connecting_for_both_save_positions() {
    for command in [
        vec![
            "openlogi", "api", "--save", "lighting", "--device", "test", "--status",
        ],
        vec![
            "openlogi", "api", "lighting", "--device", "test", "--status", "--save",
        ],
        vec![
            "openlogi", "api", "lighting", "--device", "test", "--set", "toggle",
        ],
    ] {
        let parsed = crate::Cli::try_parse_from(command).unwrap();
        let Some(crate::cmd::Command::Api(args)) = parsed.cmd else {
            panic!("expected API command")
        };
        let error = serde_json::to_value(args.validate().expect_err("invalid operation")).unwrap();
        assert_eq!(error["code"], "invalid_value");
    }
}

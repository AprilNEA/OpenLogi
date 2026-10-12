use std::fs;

use openlogi_core::config::Config;
use openlogi_core::device::{DeviceModelInfo, DeviceTransports};
use openlogi_core::paths::CONFIG_FILE;

use super::*;

fn identified_snapshot() -> AgentSnapshot {
    let mut snapshot = snapshot();
    snapshot.inventory[0].paired[0].model_info = Some(DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [0x12, 0x34, 0x56, 0x78],
        transports: DeviceTransports::default(),
        model_ids: [0xb042, 0, 0],
        extended_model_id: 0,
    });
    snapshot
}

fn dpi_command() -> ApiCommand {
    ApiCommand::Dpi {
        device: route().to_string(),
        set: Some(1200),
    }
}

fn dpi_write() -> Step {
    Box::new(|request| {
        let AgentRequest::SetDpi {
            route: requested,
            dpi,
        } = request
        else {
            panic!("unexpected {request:?}")
        };
        assert_eq!(requested, route());
        assert_eq!(dpi, Dpi::from(1200));
        AgentResponse::SetDpi(Ok(()))
    })
}

#[tokio::test]
async fn saves_verified_dpi_then_reloads_preserving_comments_and_other_devices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    fs::write(
        &path,
        "# keep me\nschema_version = 6\n[devices.\"unit:87654321\"]\ndpi = 2600\n",
    )
    .unwrap();
    let reload_path = path.clone();
    let result = run_script_at(
        identified_snapshot(),
        dpi_command(),
        vec![
            dpi_read(400),
            dpi_write(),
            dpi_read(1200),
            Box::new(move |request| {
                assert!(matches!(request, AgentRequest::ReloadConfig {}));
                let saved = Config::load_from_path(&reload_path).unwrap();
                assert_eq!(saved.dpi("unit:12345678"), Some(1200.into()));
                assert_eq!(saved.dpi("unit:87654321"), Some(2600.into()));
                assert_eq!(saved.devices.len(), 2);
                assert!(
                    fs::read_to_string(reload_path)
                        .unwrap()
                        .contains("# keep me")
                );
                AgentResponse::ReloadConfig(Ok(()))
            }),
        ],
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["persistence"], "saved");
}

#[tokio::test]
async fn readback_mismatch_never_saves_or_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let result = run_script_at(
        identified_snapshot(),
        dpi_command(),
        vec![dpi_read(400), dpi_write(), dpi_read(2600)],
        Some(&path),
    )
    .await;
    assert_eq!(result["error"]["code"], "readback_mismatch");
    assert!(!path.exists());
}

#[tokio::test]
async fn concurrent_edit_after_hardware_write_is_not_overwritten_or_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let edit_path = path.clone();
    let external = "schema_version = 6\nselected_device = \"external\"\n";
    let result = run_script_at(
        identified_snapshot(),
        dpi_command(),
        vec![
            dpi_read(400),
            Box::new(move |request| {
                fs::write(edit_path, external).unwrap();
                dpi_write()(request)
            }),
            dpi_read(1200),
        ],
        Some(&path),
    )
    .await;
    assert_eq!(result["error"]["code"], "config_conflict");
    assert_eq!(result["error"]["persistence"], "not_saved");
    assert_eq!(fs::read_to_string(path).unwrap(), external);
}

#[tokio::test]
async fn failed_reload_reports_that_the_setting_is_already_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let result = run_script_at(
        identified_snapshot(),
        dpi_command(),
        vec![
            dpi_read(400),
            dpi_write(),
            dpi_read(1200),
            Box::new(|request| {
                assert!(matches!(request, AgentRequest::ReloadConfig {}));
                AgentResponse::ReloadConfig(Err(openlogi_ipc::ConfigReloadError {
                    message: "reload refused".into(),
                }))
            }),
        ],
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "config_reload_failed");
    assert_eq!(result["error"]["persistence"], "saved");
    assert_eq!(
        Config::load_from_path(&path).unwrap().dpi("unit:12345678"),
        Some(1200.into())
    );
}

#[tokio::test]
async fn rejects_unidentified_devices_reads_and_link_overrides_before_hardware_io() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let unidentified = run_script_at(snapshot(), dpi_command(), vec![], Some(&path)).await;
    assert_eq!(unidentified["error"]["code"], "identity_unavailable");
    let read = run_script_at(
        identified_snapshot(),
        ApiCommand::Dpi {
            device: route().to_string(),
            set: None,
        },
        vec![],
        Some(&path),
    )
    .await;
    assert_eq!(read["error"]["code"], "invalid_value");
    assert!(!path.exists());
    let original = "schema_version = 6\n[devices.\"unit:12345678\".links.\"receiver:test-receiver:slot:3\".overrides]\ndpi = 2600\n";
    fs::write(&path, original).unwrap();
    let overridden = run_script_at(identified_snapshot(), dpi_command(), vec![], Some(&path)).await;
    assert_eq!(overridden["error"]["code"], "link_override");
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[tokio::test]
async fn smartshift_save_validates_the_floor_and_preserves_torque() {
    for threshold in [7, 8, 255] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let before = SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::try_from(threshold).unwrap(),
            tunable_torque: Some(TunableTorque::try_new(83).unwrap()),
        };
        let after = SmartShiftStatus {
            mode: SmartShiftMode::Free,
            ..before
        };
        let mut steps: Vec<Step> = vec![Box::new(move |request| {
            assert!(matches!(request, AgentRequest::ReadSmartshift { .. }));
            AgentResponse::ReadSmartshift(Ok(before))
        })];
        if threshold >= 8 {
            steps.push(Box::new(move |request| {
                let AgentRequest::SetSmartshift { status, .. } = request else {
                    panic!("unexpected {request:?}")
                };
                assert_eq!(status, after);
                AgentResponse::SetSmartshift(Ok(()))
            }));
            steps.push(Box::new(move |request| {
                assert!(matches!(request, AgentRequest::ReadSmartshift { .. }));
                AgentResponse::ReadSmartshift(Ok(after))
            }));
            steps.push(Box::new(|request| {
                assert!(matches!(request, AgentRequest::ReloadConfig {}));
                AgentResponse::ReloadConfig(Ok(()))
            }));
        }
        let result = run_script_at(
            identified_snapshot(),
            ApiCommand::Smartshift(SmartshiftArgs {
                device: route().to_string(),
                mode: Some(WheelMode::Free),
                auto_disengage: None,
            }),
            steps,
            Some(&path),
        )
        .await;
        if threshold == 7 {
            assert_eq!(result["error"]["code"], "invalid_value");
            assert!(!path.exists());
        } else {
            assert_eq!(result["data"]["persistence"], "saved");
            let saved = Config::load_from_path(&path)
                .unwrap()
                .smartshift("unit:12345678")
                .unwrap();
            assert_eq!(saved.mode, openlogi_core::config::WheelMode::Free);
            assert_eq!(u8::from(saved.auto_disengage), threshold);
            assert_eq!(saved.tunable_torque.unwrap().into_inner(), 83);
        }
    }
}

#[tokio::test]
async fn saves_fn_lock_to_the_existing_legacy_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    fs::write(&path, "schema_version = 6\n[devices.\"receiver:test-receiver:slot:3\"]\ndpi = 400\nfn_lock = true\n").unwrap();
    let result = run_script_at(
        identified_snapshot(),
        ApiCommand::FnLock {
            device: route().to_string(),
            set: Some(Switch::Off),
        },
        vec![
            Box::new(|request| {
                let AgentRequest::SetFnLock { fn_lock, .. } = request else {
                    panic!("unexpected {request:?}")
                };
                assert!(!fn_lock);
                AgentResponse::SetFnLock(Ok(FnLockState {
                    fn_lock: false,
                    default_fn_lock: true,
                }))
            }),
            Box::new(|request| {
                assert!(matches!(request, AgentRequest::ReloadConfig {}));
                AgentResponse::ReloadConfig(Ok(()))
            }),
        ],
        Some(&path),
    )
    .await;
    assert_eq!(result["data"]["persistence"], "saved");
    let config = Config::load_from_path(&path).unwrap();
    assert_eq!(config.fn_lock("receiver:test-receiver:slot:3"), Some(false));
    assert_eq!(
        config.dpi("receiver:test-receiver:slot:3"),
        Some(400.into())
    );
    assert_eq!(config.devices.len(), 1);
}

#[test]
fn parser_accepts_save_after_the_setting() {
    let cli = crate::Cli::try_parse_from([
        "openlogi", "api", "dpi", "--device", "id", "--set", "1200", "--save",
    ])
    .unwrap();
    assert!(matches!(
        cli.cmd,
        Some(crate::cmd::Command::Api(ApiArgs { save: true, .. }))
    ));
}

#[tokio::test]
async fn lost_reload_acknowledgement_keeps_the_saved_preference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let stop = Arc::new(Mutex::new(Some(stop)));
    let client = in_memory_agent(
        move |request| {
            let response = match request {
                AgentRequest::Snapshot {} => AgentResponse::Snapshot(identified_snapshot()),
                AgentRequest::SetFnLock { fn_lock, .. } => {
                    assert!(fn_lock);
                    AgentResponse::SetFnLock(Ok(FnLockState {
                        fn_lock: true,
                        default_fn_lock: false,
                    }))
                }
                AgentRequest::ReloadConfig {} => {
                    stop.lock().unwrap().take().unwrap().send(()).unwrap();
                    return Box::pin(pending());
                }
                other => panic!("unexpected {other:?}"),
            };
            Box::pin(async move { Ok(response) })
        },
        async {
            stopped.await.unwrap();
        },
    );
    let result = envelope(
        execute(
            &client,
            ApiCommand::FnLock {
                device: route().to_string(),
                set: Some(Switch::On),
            },
            Some(&path),
        )
        .await,
    );
    assert_eq!(result["error"]["code"], "config_reload_unknown");
    assert_eq!(result["error"]["persistence"], "saved");
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .fn_lock("unit:12345678"),
        Some(true)
    );
}

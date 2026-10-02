use std::collections::VecDeque;
use std::future::pending;
use std::sync::{Arc, Mutex};

use clap::Parser as _;
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use openlogi_core::hid::{
    DeviceRoute, DpiCapabilities, DpiInfo, FnLockState, SmartShiftAutoDisengage, SmartShiftStatus,
    TunableTorque,
};
use openlogi_ipc::client::{ConnectError, ProtocolSkew};
use openlogi_ipc::testing::in_memory_agent;
use openlogi_ipc::{AgentRequest, AgentResponse, AgentStatus, ForegroundApps, InventoryHealth};

use super::*;

type Step = Box<dyn FnOnce(AgentRequest) -> AgentResponse + Send>;

fn snapshot() -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: false,
            hook_installed: false,
            launch_at_login: true,
            inventory: InventoryHealth::Ready,
            protocol_version: 0,
            agent_version: "test-agent".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: "Test receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("test-receiver".into()),
            },
            paired: vec![PairedDevice {
                slot: 3,
                codename: Some("Test mouse".into()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: None,
            }],
        }],
        standalone: Vec::new(),
        camera_active: false,
        device_selection: None,
        pairing: None,
        foreground: ForegroundApps {
            current: None,
            recent: Vec::new(),
        },
    }
}

fn route() -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: "test-receiver".into(),
        slot: 3,
    }
}

async fn run_script(snapshot: AgentSnapshot, command: ApiCommand, steps: Vec<Step>) -> Value {
    let mut queue: VecDeque<Step> = VecDeque::new();
    queue.push_back(Box::new(move |request| {
        assert!(matches!(request, AgentRequest::Snapshot {}));
        AgentResponse::Snapshot(snapshot)
    }));
    queue.extend(steps);
    let queue = Arc::new(Mutex::new(queue));
    let calls = queue.clone();
    let client = in_memory_agent(
        move |request| {
            let step = calls.lock().unwrap().pop_front().expect("unexpected RPC");
            let response = step(request);
            Box::pin(async move { Ok(response) })
        },
        pending(),
    );
    let result = envelope(execute(&client, command).await);
    assert!(
        queue.lock().unwrap().is_empty(),
        "expected RPC was not sent"
    );
    result
}

fn dpi_read(current: u16) -> Step {
    Box::new(move |request| {
        let AgentRequest::ReadDpi { route: requested } = request else {
            panic!("expected DPI read, got {request:?}");
        };
        assert_eq!(requested, route());
        AgentResponse::ReadDpi(Ok(DpiInfo {
            current: current.into(),
            capabilities: DpiCapabilities::new(vec![400, 1200, 2600]).unwrap(),
        }))
    })
}

#[tokio::test]
async fn inventory_json_keeps_unknowns_and_does_not_open_devices() {
    let result = run_script(snapshot(), ApiCommand::Devices, vec![]).await;
    assert_eq!(
        result,
        json!({
            "schema_version": 1,
            "ok": true,
            "data": {
                "inventory": "ready",
                "devices": [{
                    "id": "slot 3 on receiver test-receiver",
                    "name": "Test mouse", "kind": "mouse", "online": true,
                    "battery": null, "capabilities": null, "light_capabilities": null,
                }],
            },
        })
    );
    for (health, label) in [
        (InventoryHealth::Ready, "ready"),
        (InventoryHealth::Scanning, "scanning"),
        (InventoryHealth::Unavailable, "unavailable"),
    ] {
        let mut snapshot = snapshot();
        snapshot.status.inventory = health;
        snapshot.inventory.clear();
        let result = run_script(snapshot, ApiCommand::Devices, vec![]).await;
        assert_eq!(result["data"], json!({ "inventory": label, "devices": [] }));
        assert_eq!(result["ok"], true);
    }
}

#[tokio::test]
async fn inventory_projects_mixed_devices_without_unrelated_private_state() {
    use openlogi_core::hid::PasskeyMethod;
    use openlogi_fixture::{CANONICAL_DEVICE_PROFILE_JSON, DeviceProfile};
    use openlogi_ipc::PairingPhase;

    let profile: DeviceProfile = serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).unwrap();
    let mut snapshot = snapshot();
    snapshot.inventory = profile.inventories;
    snapshot.standalone = profile.standalone;
    snapshot.inventory[0].paired[0]
        .model_info
        .as_mut()
        .unwrap()
        .serial_number = Some("PRIVATE-MODEL-SERIAL".into());
    snapshot.pairing = Some(PairingPhase::Passkey(PasskeyMethod::Keyboard(
        "123456".into(),
    )));
    let result = run_script(snapshot, ApiCommand::Devices, vec![]).await;
    let devices = result["data"]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 5);
    assert_eq!(
        devices[0]["battery"],
        json!({
            "percentage": 80, "level": "good", "status": "discharging",
            "freshness": "current",
        })
    );
    assert_eq!(devices[0]["capabilities"]["pointer"], true);
    assert_eq!(devices[1]["online"], false);
    assert_eq!(devices[1]["battery"], Value::Null);
    assert_eq!(devices[3]["id"], "direct 046d:b020");
    assert_eq!(devices[3]["battery"]["percentage"], 55);
    assert_eq!(devices[4]["kind"], "light");
    assert_eq!(devices[4]["battery"], Value::Null);
    assert_eq!(
        devices[4]["light_capabilities"]["temperature"],
        json!({
            "min": 2700, "max": 6500, "step": 100, "unit": "kelvin",
        })
    );
    let encoded = result.to_string();
    assert!(!encoded.contains("PRIVATE-MODEL-SERIAL"));
    assert!(!encoded.contains("123456"));
    assert!(!encoded.contains("foreground"));
}

#[tokio::test]
async fn status_has_a_separate_explicit_json_contract() {
    let result = run_script(snapshot(), ApiCommand::Status, vec![]).await;
    assert_eq!(
        result["data"],
        json!({
            "agent_version": "test-agent", "inventory": "ready",
            "accessibility_granted": false, "input_monitoring_granted": true,
            "hook_installed": false, "hid_open_failures": false, "launch_at_login": true,
        })
    );
}

#[tokio::test]
async fn selection_failures_never_issue_a_device_rpc() {
    let mut duplicate = snapshot();
    duplicate.inventory.push(duplicate.inventory[0].clone());
    let mut offline = snapshot();
    offline.inventory[0].paired[0].online = false;
    let mut scanning = snapshot();
    scanning.status.inventory = InventoryHealth::Scanning;
    for (snapshot, id, code) in [
        (duplicate, route().to_string(), "ambiguous_device"),
        (offline, route().to_string(), "device_offline"),
        (scanning, route().to_string(), "inventory_not_ready"),
        (snapshot(), "Test mouse".into(), "device_not_found"),
        (
            snapshot(),
            "bolt:test-receiver:1".into(),
            "device_not_found",
        ),
    ] {
        let result = run_script(
            snapshot,
            ApiCommand::Dpi {
                device: id,
                set: Some(1200),
            },
            vec![],
        )
        .await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], code);
    }
}

#[tokio::test]
async fn dpi_validates_supported_values_before_writing_and_checks_readback() {
    let command = |set| ApiCommand::Dpi {
        device: route().to_string(),
        set,
    };
    let read = run_script(snapshot(), command(None), vec![dpi_read(400)]).await;
    assert_eq!(
        read["data"],
        json!({
            "device": "slot 3 on receiver test-receiver", "persistence": "not_saved",
            "current": 400, "supported": [400, 1200, 2600],
        })
    );
    let invalid = run_script(snapshot(), command(Some(800)), vec![dpi_read(400)]).await;
    assert_eq!(invalid["error"]["code"], "invalid_value");
    for (readback, success) in [(1200, true), (400, false)] {
        let result = run_script(
            snapshot(),
            command(Some(1200)),
            vec![
                dpi_read(400),
                Box::new(|request| {
                    let AgentRequest::SetDpi {
                        route: requested,
                        dpi,
                    } = request
                    else {
                        panic!("expected DPI write, got {request:?}");
                    };
                    assert_eq!(requested, route());
                    assert_eq!(dpi, Dpi::from(1200));
                    AgentResponse::SetDpi(Ok(()))
                }),
                dpi_read(readback),
            ],
        )
        .await;
        assert_eq!(result["ok"], success);
        if success {
            assert_eq!(result["data"]["current"], 1200);
        } else {
            assert_eq!(result["error"]["code"], "readback_mismatch");
        }
    }
}

#[tokio::test]
async fn smartshift_preserves_unspecified_fields_and_reads_back() {
    let before = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::try_from(37).unwrap(),
        tunable_torque: Some(TunableTorque::try_new(83).unwrap()),
    };
    let after = SmartShiftStatus {
        mode: SmartShiftMode::Free,
        ..before
    };
    let read = |state| -> Step {
        Box::new(move |request| {
            let AgentRequest::ReadSmartshift { route: requested } = request else {
                panic!("expected SmartShift read, got {request:?}");
            };
            assert_eq!(requested, route());
            AgentResponse::ReadSmartshift(Ok(state))
        })
    };
    let result = run_script(
        snapshot(),
        ApiCommand::Smartshift(SmartshiftArgs {
            device: route().to_string(),
            mode: Some(WheelMode::Free),
            auto_disengage: None,
        }),
        vec![
            read(before),
            Box::new(move |request| {
                let AgentRequest::SetSmartshift {
                    route: requested,
                    status,
                } = request
                else {
                    panic!("expected SmartShift write, got {request:?}");
                };
                assert_eq!(requested, route());
                assert_eq!(status, after);
                AgentResponse::SetSmartshift(Ok(()))
            }),
            read(after),
        ],
    )
    .await;
    assert_eq!(
        result["data"],
        json!({
            "device": "slot 3 on receiver test-receiver", "persistence": "not_saved",
            "mode": "free", "auto_disengage": 37, "tunable_torque": 83,
        })
    );
}

#[tokio::test]
async fn smartshift_threshold_only_preserves_mode_and_absent_torque() {
    let before = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::try_from(37).unwrap(),
        tunable_torque: None,
    };
    for (threshold, readback) in [(1, 1), (254, 254), (255, 255), (255, 37)] {
        let result = run_script(
            snapshot(),
            ApiCommand::Smartshift(SmartshiftArgs {
                device: route().to_string(),
                mode: None,
                auto_disengage: NonZeroU8::new(threshold),
            }),
            vec![
                Box::new(move |request| {
                    assert!(matches!(request, AgentRequest::ReadSmartshift { .. }));
                    AgentResponse::ReadSmartshift(Ok(before))
                }),
                Box::new(move |request| {
                    let AgentRequest::SetSmartshift { status, .. } = request else {
                        panic!("expected SmartShift write, got {request:?}");
                    };
                    assert_eq!(status.mode, SmartShiftMode::Ratchet);
                    assert_eq!(u8::from(status.auto_disengage), threshold);
                    assert_eq!(status.tunable_torque, None);
                    AgentResponse::SetSmartshift(Ok(()))
                }),
                Box::new(move |request| {
                    assert!(matches!(request, AgentRequest::ReadSmartshift { .. }));
                    AgentResponse::ReadSmartshift(Ok(SmartShiftStatus {
                        auto_disengage: SmartShiftAutoDisengage::try_from(readback).unwrap(),
                        ..before
                    }))
                }),
            ],
        )
        .await;
        if threshold == readback {
            assert_eq!(result["data"]["mode"], "ratchet");
            assert_eq!(result["data"]["auto_disengage"], threshold);
            assert_eq!(result["data"]["tunable_torque"], Value::Null);
        } else {
            assert_eq!(result["error"]["code"], "readback_mismatch");
        }
    }
}

#[tokio::test]
async fn read_only_settings_never_send_writes() {
    let smartshift = run_script(
        snapshot(),
        ApiCommand::Smartshift(SmartshiftArgs {
            device: route().to_string(),
            mode: None,
            auto_disengage: None,
        }),
        vec![Box::new(|request| {
            assert!(matches!(request, AgentRequest::ReadSmartshift { .. }));
            AgentResponse::ReadSmartshift(Ok(SmartShiftStatus {
                mode: SmartShiftMode::Free,
                auto_disengage: SmartShiftAutoDisengage::Permanent,
                tunable_torque: None,
            }))
        })],
    )
    .await;
    assert_eq!(smartshift["data"]["mode"], "free");
    assert_eq!(smartshift["data"]["auto_disengage"], 255);
    let fn_lock = run_script(
        snapshot(),
        ApiCommand::FnLock {
            device: route().to_string(),
            set: None,
        },
        vec![Box::new(|request| {
            assert!(matches!(request, AgentRequest::ReadFnLock { .. }));
            AgentResponse::ReadFnLock(Ok(FnLockState {
                fn_lock: false,
                default_fn_lock: true,
            }))
        })],
    )
    .await;
    assert_eq!(fn_lock["data"]["fn_lock"], false);
    assert_eq!(fn_lock["data"]["default_fn_lock"], true);
}

#[tokio::test]
async fn device_write_failure_is_not_retried_or_reported_as_success() {
    let result = run_script(
        snapshot(),
        ApiCommand::Dpi {
            device: route().to_string(),
            set: Some(1200),
        },
        vec![
            dpi_read(400),
            Box::new(|request| {
                assert!(matches!(request, AgentRequest::SetDpi { .. }));
                AgentResponse::SetDpi(Err(WriteError::DeviceUnreachable { index: 3 }))
            }),
        ],
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "device_offline");
}

#[tokio::test]
async fn fn_lock_uses_explicit_state_and_returns_firmware_echo() {
    for (set, enabled, observed) in [
        (Switch::On, true, true),
        (Switch::Off, false, false),
        (Switch::On, true, false),
        (Switch::Off, false, true),
    ] {
        let result = run_script(
            snapshot(),
            ApiCommand::FnLock {
                device: route().to_string(),
                set: Some(set),
            },
            vec![Box::new(move |request| {
                let AgentRequest::SetFnLock {
                    route: requested,
                    fn_lock,
                } = request
                else {
                    panic!("expected Fn-lock write, got {request:?}");
                };
                assert_eq!(requested, route());
                assert_eq!(fn_lock, enabled);
                AgentResponse::SetFnLock(Ok(FnLockState {
                    fn_lock: observed,
                    default_fn_lock: false,
                }))
            })],
        )
        .await;
        if observed == enabled {
            assert_eq!(result["ok"], true);
            assert_eq!(result["data"]["fn_lock"], enabled);
            assert_eq!(result["data"]["default_fn_lock"], false);
        } else {
            assert_eq!(result["ok"], false);
            assert_eq!(result["error"]["code"], "readback_mismatch");
            assert!(result.get("data").is_none());
        }
    }
}

#[tokio::test]
async fn unsupported_device_error_survives_the_json_boundary_without_a_write() {
    let result = run_script(
        snapshot(),
        ApiCommand::Dpi {
            device: route().to_string(),
            set: Some(1200),
        },
        vec![Box::new(|request| {
            assert!(matches!(request, AgentRequest::ReadDpi { .. }));
            AgentResponse::ReadDpi(Err(WriteError::FeatureUnsupported {
                feature_hex: 0x2201,
            }))
        })],
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "unsupported_feature");
}

#[test]
fn connection_failures_have_stable_codes() {
    for (error, code) in [
        (
            ConnectError::Endpoint(io::Error::from(io::ErrorKind::NotFound)),
            "agent_unavailable",
        ),
        (
            ConnectError::Skew(ProtocolSkew::AgentOlder { agent: 0 }),
            "version_mismatch",
        ),
        (ConnectError::Timeout, "timeout"),
    ] {
        let result = envelope(Err(error.into()));
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], code);
        assert!(result.get("data").is_none());
    }
}

#[test]
fn parser_requires_an_id_and_rejects_out_of_range_values() {
    for args in [
        vec!["openlogi", "api", "dpi", "--set", "1200"],
        vec!["openlogi", "api", "dpi", "--device", "id", "--set", "65536"],
        vec![
            "openlogi",
            "api",
            "smartshift",
            "--device",
            "id",
            "--auto-disengage",
            "0",
        ],
        vec![
            "openlogi",
            "api",
            "smartshift",
            "--device",
            "id",
            "--auto-disengage",
            "256",
        ],
    ] {
        crate::Cli::try_parse_from(args).expect_err("invalid automation arguments");
    }
    for value in ["1", "254", "255"] {
        crate::Cli::try_parse_from([
            "openlogi",
            "api",
            "smartshift",
            "--device",
            "id",
            "--auto-disengage",
            value,
        ])
        .expect("threshold boundaries and permanent ratchet parse");
    }
}

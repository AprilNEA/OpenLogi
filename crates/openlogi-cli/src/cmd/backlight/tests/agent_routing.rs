use std::collections::VecDeque;
use std::future::pending;
use std::sync::{Arc, Mutex};

use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use openlogi_core::hid::WriteError;
use openlogi_ipc::client::ProtocolSkew;
use openlogi_ipc::testing::{Answer, in_memory_agent};
use openlogi_ipc::{AgentRequest, AgentResponse, AgentStatus, ForegroundApps};

use super::*;

type Step = Box<dyn FnOnce(AgentRequest) -> Answer + Send>;

fn snapshot() -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: false,
            hook_installed: false,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: 0,
            agent_version: "test-agent".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
        },
        inventory: vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: "Test Bolt receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("test-receiver".into()),
            },
            paired: vec![PairedDevice {
                slot: 3,
                codename: Some("Test keyboard".into()),
                wpid: None,
                kind: DeviceKind::Keyboard,
                online: true,
                battery: None,
                model_info: None,
                capabilities: Some(Capabilities::from_feature_ids(&[0x1982])),
                settings_revision: 0,
            }],
        }],
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps {
            current: None,
            recent: Vec::new(),
        },
        lights: Vec::new(),
    }
}

fn route() -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: "test-receiver".into(),
        slot: 3,
    }
}

fn state() -> BacklightState {
    BacklightState {
        enabled: true,
        mode: BacklightMode::Automatic,
        status: BacklightStatus::AlsAutomatic,
        current_level: 2,
        nb_levels: 8,
        power_save: true,
        duration: None,
        controls: BacklightControls::AUTOMATIC
            | BacklightControls::LEVEL
            | BacklightControls::POWER_SAVE,
    }
}

fn reply(f: impl FnOnce(AgentRequest) -> AgentResponse + Send + 'static) -> Step {
    Box::new(move |request| {
        let response = f(request);
        Box::pin(async move { Ok(response) })
    })
}

fn read(route: DeviceRoute, value: BacklightState) -> Step {
    reply(move |request| {
        let AgentRequest::ReadBacklight { route: actual } = request else {
            panic!("expected read, got {request:?}");
        };
        assert_eq!(actual, route);
        AgentResponse::ReadBacklight(Ok(value))
    })
}

async fn script(args: BacklightArgs, steps: Vec<Step>) -> (Result<()>, String) {
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let calls = queue.clone();
    let client = in_memory_agent(
        move |request| calls.lock().unwrap().pop_front().expect("unexpected RPC")(request),
        pending(),
    );
    let mut out = Vec::new();
    let result = execute(args, Ok(client), &mut out).await;
    assert!(queue.lock().unwrap().is_empty(), "missing expected RPC");
    (result, String::from_utf8(out).unwrap())
}

async fn run(
    snapshot: AgentSnapshot,
    action: Option<BacklightAction>,
    name: Option<&str>,
    mut steps: Vec<Step>,
) -> (Result<()>, String) {
    steps.insert(
        0,
        reply(move |request| {
            assert!(matches!(request, AgentRequest::Snapshot {}));
            AgentResponse::Snapshot(snapshot)
        }),
    );
    script(
        BacklightArgs {
            device: name.map(str::to_owned),
            action,
        },
        steps,
    )
    .await
}

fn failure(result: Result<()>, out: &str, message: &str) {
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains(message),
        "{error:?} does not contain {message:?}"
    );
    assert!(!out.contains('✓'), "failure printed success: {out}");
    assert!(
        !out.contains("read-back:"),
        "failure printed a confirmed state: {out}"
    );
}

#[tokio::test]
async fn bare_and_status_read_exact_agent_routes_without_writing_or_arming() {
    for (pid, uid, slot, expected) in [
        (0xc548, Some("test-receiver"), 3, route()),
        (
            0xc52b,
            Some("test-unifying"),
            4,
            DeviceRoute::Unifying {
                receiver_uid: "test-unifying".into(),
                slot: 4,
            },
        ),
        (
            0xb35b,
            None,
            255,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb35b,
            },
        ),
    ] {
        for action in [None, Some(BacklightAction::Status)] {
            let mut snapshot = snapshot();
            snapshot.inventory[0].receiver.product_id = pid;
            snapshot.inventory[0].receiver.unique_id = uid.map(str::to_owned);
            snapshot.inventory[0].paired[0].slot = slot;
            let (result, out) = run(
                snapshot,
                action,
                None,
                vec![read(expected.clone(), state())],
            )
            .await;
            result.unwrap();
            assert!(out.contains(&format!("device: Test keyboard ({expected})")));
            assert!(out.contains("level=2/7 battery-saving=true"));
            assert!(out.contains("offers: on/off, auto, level, battery-saving"));
            assert!(!out.contains('✓'));
        }
    }
}

#[tokio::test]
async fn every_write_sends_one_exact_change_and_prints_the_agents_confirmed_state() {
    for (action, change) in [
        (BacklightAction::On, BacklightChange::Enabled(true)),
        (BacklightAction::Off, BacklightChange::Enabled(false)),
        (
            BacklightAction::Auto { set: Switch::On },
            BacklightChange::Automatic(true),
        ),
        (
            BacklightAction::Auto { set: Switch::Off },
            BacklightChange::Automatic(false),
        ),
        (
            BacklightAction::Level { level: 3 },
            BacklightChange::Level(3),
        ),
        (
            BacklightAction::BatterySaving { set: Switch::On },
            BacklightChange::PowerSave(true),
        ),
        (
            BacklightAction::BatterySaving { set: Switch::Off },
            BacklightChange::PowerSave(false),
        ),
    ] {
        // The agent owns confirmation. An incidental brightness change must
        // appear verbatim, not be replaced by the CLI's prediction of a write.
        let reported_level = match change {
            BacklightChange::Level(level) => level,
            BacklightChange::Enabled(false) => 0,
            _ => 6,
        };
        let after = BacklightState {
            current_level: reported_level,
            ..state().changed(change)
        };
        let (result, out) = run(
            snapshot(),
            Some(action),
            Some("KEYbOARD"),
            vec![
                read(route(), state()),
                reply(move |request| {
                    let AgentRequest::SetBacklight {
                        route: actual,
                        change: actual_change,
                    } = request
                    else {
                        panic!("expected write, got {request:?}");
                    };
                    assert_eq!(actual, route());
                    assert_eq!(actual_change, change);
                    AgentResponse::SetBacklight(Ok(after))
                }),
            ],
        )
        .await;
        result.unwrap();
        assert!(out.contains("read-back:"));
        let readback = out
            .lines()
            .find(|line| line.contains("read-back:"))
            .unwrap();
        assert!(readback.contains(&format!("level={reported_level}/7")));
        assert!(out.contains(&format!("✓ {change}")));
    }
}

#[tokio::test]
async fn selection_refusals_never_issue_device_rpcs() {
    let mut empty = snapshot();
    empty.inventory.clear();
    let mut offline = snapshot();
    offline.inventory[0].paired[0].online = false;
    let mut duplicate = snapshot();
    duplicate.inventory.push(duplicate.inventory[0].clone());
    let mut unsupported = snapshot();
    unsupported.inventory[0].paired[0].capabilities = Some(Capabilities::default());
    let mut unknown = snapshot();
    unknown.inventory[0].paired[0].capabilities = None;
    let mut no_route = snapshot();
    no_route.inventory[0].receiver.unique_id = None;
    let mut scanning = snapshot();
    scanning.status.inventory = InventoryHealth::Scanning;
    let mut unavailable = snapshot();
    unavailable.status.inventory = InventoryHealth::Unavailable;
    for (snapshot, query, message) in [
        (empty, None, "no matching"),
        (offline, Some("Test"), "no matching"),
        (snapshot(), Some("missing"), "no matching"),
        (duplicate.clone(), Some("Test"), "ambiguous"),
        (duplicate, None, "ambiguous"),
        (unsupported.clone(), None, "no matching"),
        (unsupported, Some("Test"), "does not advertise"),
        (unknown, None, "measured backlight support"),
        (no_route, Some("Test"), "no resolvable agent route"),
        (scanning, None, "inventory is not ready"),
        (unavailable, Some("Test"), "inventory is not ready"),
    ] {
        let (result, out) = run(snapshot, Some(BacklightAction::Off), query, vec![]).await;
        failure(result, &out, message);
    }
}

#[tokio::test]
async fn unknown_support_can_be_read_by_explicit_name_but_never_locally_probed() {
    let mut snapshot = snapshot();
    snapshot.inventory[0].paired[0].capabilities = None;
    let (result, _) = run(
        snapshot.clone(),
        None,
        Some("keyboard"),
        vec![read(route(), state())],
    )
    .await;
    result.unwrap();
    let (result, out) = run(
        snapshot,
        Some(BacklightAction::Off),
        Some("keyboard"),
        vec![reply(|request| {
            assert!(matches!(request, AgentRequest::ReadBacklight { .. }));
            AgentResponse::ReadBacklight(Err(WriteError::FeatureUnsupported {
                feature_hex: 0x1982,
            }))
        })],
    )
    .await;
    failure(result, &out, "read backlight state");
}

#[tokio::test]
async fn explicit_names_do_not_disambiguate_colliding_direct_routes() {
    let mut snapshot = snapshot();
    snapshot.inventory[0].receiver.product_id = 0xb35b;
    snapshot.inventory[0].receiver.unique_id = None;
    snapshot.inventory[0].paired[0].slot = 255;
    let mut duplicate = snapshot.inventory[0].clone();
    duplicate.paired[0].codename = Some("Other keyboard".into());
    snapshot.inventory.push(duplicate);
    let (result, out) = run(snapshot, Some(BacklightAction::Off), Some("Test"), vec![]).await;
    failure(result, &out, "ambiguous agent route");
}

#[tokio::test]
async fn automatic_selection_skips_a_mouse_and_an_offline_keyboard() {
    let mut snapshot = snapshot();
    let mut mouse = snapshot.inventory[0].paired[0].clone();
    mouse.slot = 1;
    mouse.kind = DeviceKind::Mouse;
    mouse.codename = Some("Test mouse".into());
    mouse.capabilities = Some(Capabilities::default());
    let mut sleeping = snapshot.inventory[0].paired[0].clone();
    sleeping.slot = 2;
    sleeping.online = false;
    snapshot.inventory[0].paired.splice(0..0, [mouse, sleeping]);
    let (result, _) = run(snapshot, None, None, vec![read(route(), state())]).await;
    result.unwrap();
}

#[tokio::test]
async fn failed_reads_and_failed_writes_never_retry_or_claim_confirmation() {
    for error in [
        WriteError::FeatureUnsupported {
            feature_hex: 0x1982,
        },
        WriteError::DeviceUnreachable { index: 3 },
    ] {
        let (result, out) = run(
            snapshot(),
            Some(BacklightAction::Off),
            None,
            vec![reply(move |request| {
                assert!(matches!(request, AgentRequest::ReadBacklight { .. }));
                AgentResponse::ReadBacklight(Err(error))
            })],
        )
        .await;
        failure(result, &out, "read backlight state");
    }
    for error in [
        WriteError::BacklightNotApplied {
            change: BacklightChange::Enabled(false),
        },
        WriteError::BacklightChangeUnsupported {
            change: BacklightChange::Enabled(false),
        },
        WriteError::DeviceUnreachable { index: 3 },
    ] {
        let message = error.to_string();
        let (result, out) = run(
            snapshot(),
            Some(BacklightAction::Off),
            None,
            vec![
                read(route(), state()),
                reply(move |request| {
                    assert!(matches!(request, AgentRequest::SetBacklight { .. }));
                    AgentResponse::SetBacklight(Err(error))
                }),
            ],
        )
        .await;
        failure(result, &out, &message);
    }
}

#[tokio::test]
async fn connection_refusals_do_not_fall_back_or_print_device_output() {
    for error in [
        ConnectError::Endpoint(io::Error::from(io::ErrorKind::NotFound)),
        ConnectError::Skew(ProtocolSkew::AgentOlder { agent: 0 }),
        ConnectError::Skew(ProtocolSkew::AgentNewer { agent: u32::MAX }),
        ConnectError::Timeout,
        ConnectError::Handshake(tarpc::client::RpcError::Shutdown),
    ] {
        let expected = error.to_string();
        let mut out = Vec::new();
        let result = execute(
            BacklightArgs {
                device: None,
                action: Some(BacklightAction::Off),
            },
            Err(error),
            &mut out,
        )
        .await;
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("same OPENLOGI_PROFILE"));
        assert!(error.contains("no direct hardware fallback"));
        assert!(error.contains(&expected));
        assert!(out.is_empty());
    }
}

#[tokio::test]
async fn shared_deadline_bounds_snapshot_read_and_write_without_retry() {
    // Real shared budget, no command-local timeout or duplicated constant.
    // All three stalled stages run concurrently to keep the test bounded.
    let check = |stage| async move {
        let mut steps: Vec<Step> = Vec::new();
        if stage > 0 {
            steps.push(reply(|request| {
                assert!(matches!(request, AgentRequest::Snapshot {}));
                AgentResponse::Snapshot(snapshot())
            }));
        }
        if stage > 1 {
            steps.push(read(route(), state()));
        }
        steps.push(Box::new(move |request| {
            assert!(matches!(
                (stage, request),
                (0, AgentRequest::Snapshot {})
                    | (1, AgentRequest::ReadBacklight { .. })
                    | (2, AgentRequest::SetBacklight { .. })
            ));
            Box::pin(pending())
        }));
        let (result, out) = script(
            BacklightArgs {
                device: None,
                action: Some(BacklightAction::Off),
            },
            steps,
        )
        .await;
        let error = format!("{:#}", result.as_ref().unwrap_err());
        assert!(error.contains("timed out"), "{error}");
        if stage == 2 {
            assert!(error.contains("outcome is unconfirmed"));
        }
        failure(result, &out, "timed out");
    };
    tokio::join!(check(0), check(1), check(2));
}

#[tokio::test]
async fn disconnected_rpc_does_not_retry_and_warns_about_unconfirmed_writes() {
    for stage in 0..3 {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let stop = Arc::new(Mutex::new(Some(stop)));
        let calls = Arc::new(Mutex::new(0));
        let count = calls.clone();
        let client = in_memory_agent(
            move |request| {
                let mut count = count.lock().unwrap();
                let current = *count;
                *count += 1;
                assert!(matches!(
                    (current, &request),
                    (0, AgentRequest::Snapshot {})
                        | (1, AgentRequest::ReadBacklight { .. })
                        | (2, AgentRequest::SetBacklight { .. })
                ));
                if current == stage {
                    stop.lock().unwrap().take().unwrap().send(()).unwrap();
                    return Box::pin(pending());
                }
                Box::pin(async move {
                    Ok(match request {
                        AgentRequest::Snapshot {} => AgentResponse::Snapshot(snapshot()),
                        AgentRequest::ReadBacklight { .. } => {
                            AgentResponse::ReadBacklight(Ok(state()))
                        }
                        other => panic!("unexpected RPC: {other:?}"),
                    })
                })
            },
            async {
                stopped.await.unwrap();
            },
        );
        let mut out = Vec::new();
        let result = execute(
            BacklightArgs {
                device: None,
                action: Some(BacklightAction::Off),
            },
            Ok(client),
            &mut out,
        )
        .await;
        let error = format!("{:#}", result.as_ref().unwrap_err());
        assert!(error.contains("disconnected"), "{error}");
        if stage == 2 {
            assert!(error.contains("outcome is unconfirmed"));
        }
        failure(result, &String::from_utf8(out).unwrap(), "disconnected");
        assert_eq!(*calls.lock().unwrap(), stage + 1);
    }
}

#[tokio::test]
async fn temporary_manual_warning_is_preserved_in_real_command_output() {
    let before = BacklightState {
        mode: BacklightMode::TemporaryManual,
        ..state()
    };
    let (result, out) = run(
        snapshot(),
        Some(BacklightAction::Off),
        None,
        vec![
            read(route(), before),
            reply(|_| {
                AgentResponse::SetBacklight(Ok(state().changed(BacklightChange::Enabled(false))))
            }),
        ],
    )
    .await;
    result.unwrap();
    assert!(out.contains("a mode software cannot write back"));
}

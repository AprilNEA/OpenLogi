use std::sync::{Arc, Mutex};

use clap::Parser;
use futures::StreamExt as _;
use openlogi_core::device::{DeviceKind, PairedDevice, ReceiverInfo};
use openlogi_core::hid::{DpiCapabilities, DpiInfo, SmartShiftStatus, TunableTorque, WriteError};
use openlogi_ipc::{
    AgentRequest, AgentResponse, AgentSnapshot, AgentStatus, ForegroundApps, PROTOCOL_VERSION,
};
use tarpc::server::{BaseChannel, Channel as _};

use super::*;

fn inventory() -> Vec<DeviceInventory> {
    vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "Synthetic mouse".into(),
            vendor_id: 0x046d,
            product_id: 0xb034,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: 255,
            codename: Some("Test Mouse".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        }],
    }]
}

struct DeviceState {
    dpi: DpiInfo,
    smartshift: SmartShiftStatus,
    writes: usize,
    apply: bool,
    reject: bool,
}
fn test_agent(
    inventory: Vec<DeviceInventory>,
    health: InventoryHealth,
) -> (AgentClient, Arc<Mutex<DeviceState>>) {
    let snapshot = AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: false,
            hook_installed: false,
            launch_at_login: false,
            inventory: health,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "fixture".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory,
        standalone: vec![],
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
    };
    let state = Arc::new(Mutex::new(DeviceState {
        dpi: DpiInfo {
            current: Dpi::new(800),
            capabilities: DpiCapabilities::new(vec![400, 800, 1600]).unwrap(),
        },
        smartshift: SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::try_from(20).unwrap(),
            tunable_torque: Some(TunableTorque::try_from(37).unwrap()),
        },
        writes: 0,
        apply: true,
        reject: false,
    }));
    let shared = state.clone();
    let (client, server) = tarpc::transport::channel::unbounded();
    let serve = tarpc::server::serve(move |_: tarpc::context::Context, request: AgentRequest| {
        let response = match request {
            AgentRequest::Snapshot {} => AgentResponse::Snapshot(snapshot.clone()),
            AgentRequest::ReadDpi { .. } => {
                AgentResponse::ReadDpi(Ok(shared.lock().unwrap().dpi.clone()))
            }
            AgentRequest::ReadSmartshift { .. } => {
                AgentResponse::ReadSmartshift(Ok(shared.lock().unwrap().smartshift))
            }
            AgentRequest::SetDpi { dpi, .. } => {
                let mut s = shared.lock().unwrap();
                s.writes += 1;
                if s.reject {
                    AgentResponse::SetDpi(Err(WriteError::FeatureUnsupported {
                        feature_hex: 0x2201,
                    }))
                } else {
                    if s.apply {
                        s.dpi.current = dpi;
                    }
                    AgentResponse::SetDpi(Ok(()))
                }
            }
            AgentRequest::SetSmartshift { status, .. } => {
                let mut s = shared.lock().unwrap();
                s.writes += 1;
                if s.apply {
                    s.smartshift = status;
                }
                AgentResponse::SetSmartshift(Ok(()))
            }
            _ => panic!("unexpected RPC: {request:?}"),
        };
        std::future::ready(Ok(response))
    });
    tokio::spawn(BaseChannel::with_defaults(server).execute(serve).for_each(
        |response| async move {
            tokio::spawn(response);
        },
    ));
    (
        AgentClient::new(tarpc::client::Config::default(), client).spawn(),
        state,
    )
}

fn dpi(value: Option<u16>) -> ControlCmd {
    ControlCmd::Dpi(DpiArgs {
        target: TargetArgs {
            device: "Test Mouse".into(),
        },
        set: value.and_then(NonZeroU16::new),
    })
}

#[test]
fn control_requires_an_exact_target_and_valid_values() {
    for args in [
        vec!["openlogi", "control", "dpi"],
        vec![
            "openlogi",
            "control",
            "dpi",
            "--device",
            "Test Mouse",
            "--set",
            "0",
        ],
        vec![
            "openlogi",
            "control",
            "smartshift",
            "--device",
            "Test Mouse",
            "--threshold",
            "0",
        ],
        vec![
            "openlogi",
            "control",
            "smartshift",
            "--device",
            "Test Mouse",
            "--mode",
            "unknown",
        ],
    ] {
        crate::Cli::try_parse_from(args).expect_err("invalid command must fail before connecting");
    }
    crate::Cli::try_parse_from([
        "openlogi",
        "control",
        "smartshift",
        "--device",
        "Test Mouse",
        "--threshold",
        "255",
    ])
    .unwrap();
    selected(&inventory(), "Mouse").expect_err("substrings cannot select a device");
    let mut duplicate = inventory();
    let mut second = duplicate[0].clone();
    second.paired[0].codename = Some("Other mouse".into());
    duplicate.push(second);
    selected(&duplicate, "Test Mouse")
        .expect_err("a duplicate direct route is ambiguous even when names differ");
}

#[tokio::test]
async fn reads_never_write_and_publish_versioned_json() {
    let (client, state) = test_agent(inventory(), InventoryHealth::Ready);
    let report = execute(&client, ControlCmd::Devices).await.unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["devices"][0]["id"], "direct 046d:b034");
    assert!(report.get("foreground").is_none());
    assert!(report.get("pairing").is_none());
    let report = execute(&client, dpi(None)).await.unwrap();
    assert_eq!(report["written"], false);
    assert_eq!(report["current"]["current"], 800);
    execute(
        &client,
        ControlCmd::Smartshift(SmartshiftArgs {
            target: TargetArgs {
                device: "Test Mouse".into(),
            },
            mode: None,
            threshold: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(state.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn dpi_validates_support_then_writes_once_and_checks_readback() {
    let (client, state) = test_agent(inventory(), InventoryHealth::Ready);
    execute(&client, dpi(Some(1200)))
        .await
        .expect_err("unsupported DPI must not write");
    assert_eq!(state.lock().unwrap().writes, 0);
    let report = execute(&client, dpi(Some(1600))).await.unwrap();
    assert_eq!(report["written"], true);
    assert_eq!(report["current"]["current"], 1600);
    assert_eq!(state.lock().unwrap().writes, 1);
    state.lock().unwrap().apply = false;
    execute(&client, dpi(Some(400)))
        .await
        .expect_err("acknowledgement is not read-back verification");
    assert_eq!(
        state.lock().unwrap().writes,
        2,
        "mismatched read-back must never retry a write"
    );
    state.lock().unwrap().reject = true;
    execute(&client, dpi(Some(800)))
        .await
        .expect_err("typed agent rejection must fail");
    assert_eq!(state.lock().unwrap().writes, 3);
}

#[tokio::test]
async fn unavailable_inventory_and_offline_devices_cannot_write() {
    let (client, state) = test_agent(inventory(), InventoryHealth::Scanning);
    execute(&client, dpi(Some(1600)))
        .await
        .expect_err("scanning is not an authoritative inventory");
    assert_eq!(state.lock().unwrap().writes, 0);
    let mut offline = inventory();
    offline[0].paired[0].online = false;
    let (client, state) = test_agent(offline, InventoryHealth::Ready);
    execute(&client, dpi(Some(1600)))
        .await
        .expect_err("offline targets cannot write");
    assert_eq!(state.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn smartshift_preserves_torque_and_unspecified_threshold() {
    let (client, state) = test_agent(inventory(), InventoryHealth::Ready);
    let before = state.lock().unwrap().smartshift;
    execute(
        &client,
        ControlCmd::Smartshift(SmartshiftArgs {
            target: TargetArgs {
                device: "Test Mouse".into(),
            },
            mode: Some(Mode::Free),
            threshold: None,
        }),
    )
    .await
    .unwrap();
    let after = state.lock().unwrap().smartshift;
    assert_eq!(after.mode, SmartShiftMode::Free);
    assert_eq!(after.auto_disengage, before.auto_disengage);
    assert_eq!(after.tunable_torque, before.tunable_torque);
    assert_eq!(state.lock().unwrap().writes, 1);
}

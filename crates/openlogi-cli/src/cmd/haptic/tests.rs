use std::collections::VecDeque;
use std::future::pending;
use std::sync::{Arc, Mutex};

use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};
use openlogi_core::hid::DeviceRoute;
use openlogi_ipc::client::ProtocolSkew;
use openlogi_ipc::testing::{Answer, in_memory_agent};
use openlogi_ipc::{
    AgentRequest, AgentResponse, AgentSnapshot, AgentStatus, ForegroundApps, InventoryHealth,
};

use super::*;

type Step = Box<dyn FnOnce(AgentRequest) -> Answer + Send>;

fn paired(slot: u8, name: &str, kind: DeviceKind, features: &[u16]) -> PairedDevice {
    PairedDevice {
        slot,
        codename: Some(name.into()),
        wpid: None,
        kind,
        online: true,
        battery: None,
        model_info: None,
        capabilities: Some(Capabilities::from_feature_ids(features)),
        settings_revision: 0,
        hidpp10: false,
    }
}

/// A receiver with a haptic mouse on slot 2 and a keyboard without haptics
/// on slot 3.
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
            paired: vec![
                paired(2, "MX Master 4", DeviceKind::Mouse, &[0x19b0]),
                paired(3, "MX Keys S", DeviceKind::Keyboard, &[0x1982]),
            ],
        }],
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps {
            current: None,
            recent: Vec::new(),
        },
        lights: Vec::new(),
        flow: openlogi_ipc::FlowStatus::default(),
        config_revision: openlogi_core::config::ConfigRevision::default(),
    }
}

fn slot(slot: u8) -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: "test-receiver".into(),
        slot,
    }
}

fn reply(f: impl FnOnce(AgentRequest) -> AgentResponse + Send + 'static) -> Step {
    Box::new(move |request| {
        let response = f(request);
        Box::pin(async move { Ok(response) })
    })
}

/// The agent receives `pulse` for `route` and answers `result`.
fn play(route: DeviceRoute, pulse: HapticPulse, result: Result<(), WriteError>) -> Step {
    reply(move |request| {
        let AgentRequest::PlayHaptic {
            route: actual,
            pulse: asked,
        } = request
        else {
            panic!("expected a haptic play, got {request:?}");
        };
        assert_eq!((actual, asked), (route, pulse));
        AgentResponse::PlayHaptic(result)
    })
}

async fn run(
    snapshot: AgentSnapshot,
    device: Option<&str>,
    waveform: Waveform,
    steps: Vec<Step>,
) -> (Result<()>, String) {
    let mut steps = VecDeque::from(steps);
    steps.push_front(reply(move |request| {
        assert!(matches!(request, AgentRequest::Snapshot {}));
        AgentResponse::Snapshot(snapshot)
    }));
    let queue = Arc::new(Mutex::new(steps));
    let calls = queue.clone();
    let client = in_memory_agent(
        move |request| calls.lock().unwrap().pop_front().expect("unexpected RPC")(request),
        pending(),
    );
    let mut out = Vec::new();
    let args = HapticArgs {
        device: device.map(str::to_owned),
        action: HapticAction::Play { waveform },
    };
    let result = execute(args, Ok(client), &mut out).await;
    assert!(queue.lock().unwrap().is_empty(), "missing expected RPC");
    (result, String::from_utf8(out).unwrap())
}

fn failure(result: Result<()>, out: &str, message: &str) {
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains(message),
        "{error:?} does not contain {message:?}"
    );
    assert!(!out.contains("played"), "failure printed success: {out}");
}

#[tokio::test]
async fn a_tick_plays_on_the_one_haptic_device_without_a_name() {
    let (result, out) = run(
        snapshot(),
        None,
        Waveform::Tick,
        vec![play(slot(2), HapticPulse::SubtleCollision, Ok(()))],
    )
    .await;
    result.unwrap();
    assert!(out.starts_with("played tick on MX Master 4"), "{out}");
}

#[tokio::test]
async fn confirm_plays_the_confirmation_waveform_on_the_named_device() {
    let (result, out) = run(
        snapshot(),
        Some("master"),
        Waveform::Confirm,
        vec![play(slot(2), HapticPulse::DampStateChange, Ok(()))],
    )
    .await;
    result.unwrap();
    assert!(out.starts_with("played confirm on MX Master 4"), "{out}");
}

#[tokio::test]
async fn selection_refusals_never_reach_the_agent_with_a_play() {
    let mut no_haptics = snapshot();
    no_haptics.inventory[0].paired.remove(0);
    let mut offline = snapshot();
    offline.inventory[0].paired[0].online = false;
    let mut two = snapshot();
    two.inventory[0].paired[1] = paired(3, "MX Master 4 (2)", DeviceKind::Mouse, &[0x19b0]);
    let mut scanning = snapshot();
    scanning.status.inventory = InventoryHealth::Scanning;
    for (snapshot, device, message) in [
        (no_haptics, None, "measured haptic support"),
        (offline, None, "no matching online haptic device"),
        (two.clone(), None, "ambiguous haptic device selection"),
        (two, Some("master"), "ambiguous haptic device selection"),
        (
            snapshot(),
            Some("keys"),
            "does not advertise HID++ haptic feedback",
        ),
        (scanning, None, "inventory is not ready"),
    ] {
        let (result, out) = run(snapshot, device, Waveform::Tick, vec![]).await;
        failure(result, &out, message);
    }
}

#[tokio::test]
async fn the_agents_refusals_are_reported_as_failures() {
    for (error, message) in [
        (
            WriteError::HapticWaveformUnsupported { waveform: 1 },
            "does not advertise haptic waveform 1",
        ),
        (
            WriteError::HapticsUnavailable { status: 2 },
            "haptic status 2",
        ),
        (
            WriteError::DeviceUnreachable { index: 2 },
            "did not respond",
        ),
        (
            WriteError::FeatureUnsupported {
                feature_hex: 0x19b0,
            },
            "0x19b0",
        ),
        (
            WriteError::WriteSuperseded {
                operation: openlogi_core::hid::HidppOperation::PlayHaptic,
            },
            "a newer haptic replaced this one",
        ),
    ] {
        let (result, out) = run(
            snapshot(),
            None,
            Waveform::Confirm,
            vec![play(slot(2), HapticPulse::DampStateChange, Err(error))],
        )
        .await;
        failure(result, &out, message);
    }
}

#[tokio::test]
async fn no_agent_means_no_play_and_no_fallback() {
    let mut out = Vec::new();
    let args = HapticArgs {
        device: None,
        action: HapticAction::Play {
            waveform: Waveform::Tick,
        },
    };
    let skew = ConnectError::Skew(ProtocolSkew::AgentOlder { agent: 46 });
    let error = format!(
        "{:#}",
        execute(args, Err(skew), &mut out).await.unwrap_err()
    );
    assert!(error.contains("no direct hardware fallback"), "{error}");
    assert!(out.is_empty());
}

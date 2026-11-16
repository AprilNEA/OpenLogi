use std::collections::VecDeque;
use std::future::pending;
use std::path::Path;
use std::sync::{Arc, Mutex};

use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
use openlogi_ipc::testing::{Answer, in_memory_agent};
use openlogi_ipc::{
    AgentRequest, AgentResponse, AgentSnapshot, AgentStatus, ForegroundApps, InventoryHealth,
    KeyImageError, MAX_KEY_IMAGE_BYTES,
};

use super::*;

type Step = Box<dyn FnOnce(AgentRequest) -> Answer + Send>;

/// The MX Creative Keypad, wired.
fn keypad() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Creative Keypad".into(),
            vendor_id: 0x046d,
            product_id: 0xc354,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Creative Keypad".into()),
            wpid: None,
            kind: DeviceKind::Numpad,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
            settings_revision: 0,
            hidpp10: false,
        }],
    }
}

fn keypad_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc354,
    }
}

/// A wired mouse: online, but with no LCD keys.
fn mouse() -> DeviceInventory {
    let mut mouse = keypad();
    mouse.receiver.product_id = 0xb034;
    mouse.paired[0].codename = Some("MX Master 3S".into());
    mouse.paired[0].kind = DeviceKind::Mouse;
    mouse
}

fn snapshot(inventory: Vec<DeviceInventory>) -> AgentSnapshot {
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
        inventory,
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

fn reply(f: impl FnOnce(AgentRequest) -> AgentResponse + Send + 'static) -> Step {
    Box::new(move |request| {
        let response = f(request);
        Box::pin(async move { Ok(response) })
    })
}

fn snapshot_step(inventory: Vec<DeviceInventory>) -> Step {
    reply(move |request| {
        assert!(matches!(request, AgentRequest::Snapshot {}));
        AgentResponse::Snapshot(snapshot(inventory))
    })
}

async fn script(args: KeypadArgs, steps: Vec<Step>) -> (Result<()>, String) {
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

fn middle_on_page_two() -> ButtonId {
    "page2:gridMiddle".parse().unwrap()
}

fn set_image(device: Option<&str>, file: &Path) -> KeypadArgs {
    KeypadArgs {
        device: device.map(str::to_owned),
        action: KeypadAction::SetImage {
            key: middle_on_page_two(),
            file: file.to_owned(),
        },
    }
}

fn image_file(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file
}

#[tokio::test]
async fn set_image_sends_the_file_to_the_sole_keypad() {
    let file = image_file(b"\x89PNG not checked by the CLI");
    let (result, out) = script(
        set_image(None, file.path()),
        vec![
            snapshot_step(vec![mouse(), keypad()]),
            reply(|request| {
                let AgentRequest::SetKeyImage { route, key, image } = request else {
                    panic!("expected set_key_image, got {request:?}");
                };
                assert_eq!(route, keypad_route());
                assert_eq!(key, middle_on_page_two());
                assert_eq!(image, b"\x89PNG not checked by the CLI");
                AgentResponse::SetKeyImage(Ok(()))
            }),
        ],
    )
    .await;

    result.unwrap();
    assert!(
        out.contains("page2:gridMiddle shows the image until it is cleared"),
        "{out}"
    );
}

#[tokio::test]
async fn clear_image_names_the_device_asked_for() {
    let (result, out) = script(
        KeypadArgs {
            device: Some("creative".into()),
            action: KeypadAction::ClearImage {
                key: middle_on_page_two(),
            },
        },
        vec![
            snapshot_step(vec![keypad()]),
            reply(|request| {
                let AgentRequest::ClearKeyImage { route, key } = request else {
                    panic!("expected clear_key_image, got {request:?}");
                };
                assert_eq!(route, keypad_route());
                assert_eq!(key, middle_on_page_two());
                AgentResponse::ClearKeyImage(Ok(()))
            }),
        ],
    )
    .await;

    result.unwrap();
    assert!(out.contains("shows its binding again"), "{out}");
}

#[tokio::test]
async fn the_agents_refusal_is_reported() {
    let file = image_file(b"not an image");
    let (result, _) = script(
        set_image(Some("keypad"), file.path()),
        vec![
            snapshot_step(vec![keypad()]),
            reply(|_| {
                AgentResponse::SetKeyImage(Err(KeyImageError::Undecodable(
                    "not a PNG or JPEG image".into(),
                )))
            }),
        ],
    )
    .await;

    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("could not set the image on page2:gridMiddle"));
    assert!(error.contains("not a PNG or JPEG image"), "{error}");
}

#[tokio::test]
async fn without_a_keypad_nothing_is_sent() {
    let file = image_file(b"image");
    let (result, _) = script(
        set_image(None, file.path()),
        vec![snapshot_step(vec![mouse()])],
    )
    .await;

    let error = result.unwrap_err().to_string();
    assert!(
        error.starts_with("no matching online keypad device"),
        "{error}"
    );
    assert!(error.contains("requires a device with LCD keys"), "{error}");
    assert!(error.contains("MX Master 3S"), "{error}");
}

/// While the agent is still scanning, an empty inventory says nothing about
/// the keypad yet, so nothing is chosen or sent.
#[tokio::test]
async fn a_scanning_agent_is_refused_until_its_inventory_is_ready() {
    let file = image_file(b"image");
    let (result, _) = script(
        set_image(None, file.path()),
        vec![reply(|request| {
            assert!(matches!(request, AgentRequest::Snapshot {}));
            let mut snapshot = snapshot(Vec::new());
            snapshot.status.inventory = InventoryHealth::Scanning;
            AgentResponse::Snapshot(snapshot)
        })],
    )
    .await;

    let error = result.unwrap_err().to_string();
    assert!(error.starts_with("agent inventory is not ready"), "{error}");
}

/// A file the agent would refuse, or none at all, fails before any call.
#[tokio::test]
async fn an_unsendable_file_fails_before_the_agent_is_asked() {
    let oversized = image_file(&vec![0; usize::try_from(MAX_KEY_IMAGE_BYTES).unwrap() + 1]);
    let (result, _) = script(set_image(None, oversized.path()), Vec::new()).await;
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains(&format!("larger than {MAX_KEY_IMAGE_BYTES} bytes")),
        "{error}"
    );

    let missing = Path::new("/nonexistent/openlogi-key-image.png");
    let (result, _) = script(set_image(None, missing), Vec::new()).await;
    let error = result.unwrap_err().to_string();
    assert!(error.starts_with("could not read"), "{error}");
}

/// A file far past the limit is refused from its size alone, never read
/// into memory. Sparse, so the test writes nothing to disk; Unix only, where
/// every CI filesystem keeps a sparse file sparse.
#[cfg(unix)]
#[tokio::test]
async fn a_huge_file_is_refused_without_being_read() {
    let huge = tempfile::NamedTempFile::new().unwrap();
    huge.as_file().set_len(5 << 30).unwrap();
    let (result, _) = script(set_image(None, huge.path()), Vec::new()).await;
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains(&format!("larger than {MAX_KEY_IMAGE_BYTES} bytes")),
        "{error}"
    );
}

#[test]
fn only_a_grid_key_is_accepted() {
    assert_eq!(grid_key("gridMiddle"), Ok("gridMiddle".parse().unwrap()));
    assert_eq!(
        grid_key("page3:gridBottomRight"),
        Ok("page3:gridBottomRight".parse().unwrap())
    );
    for refused in ["MiddleClick", "KeyG1", "page1:gridMiddle", "gridCentre", ""] {
        let error = grid_key(refused).unwrap_err();
        assert!(
            error.contains("is not a keypad grid key"),
            "{refused}: {error}"
        );
    }
}

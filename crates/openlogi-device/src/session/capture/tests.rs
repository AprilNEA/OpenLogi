//! A capture session hears a control from the moment arming diverts it, while
//! it still arms the controls after that one (#2349), and times what it heard
//! by when it arrived.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use openlogi_core::binding::{ButtonId, GESTURE_HOLD_FOR_SWIPE, GestureDirection};
use tokio::sync::{mpsc, oneshot, watch};

use super::*;
use crate::backend::NodeId;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
use crate::device_io_channel;
use crate::replay::ReplayChannelHandle;
use crate::reprog_controls;
use crate::session::capture_restore::CaptureSessionFailure;
use crate::session::gesture::{CaptureSpec, run_capture_session};
use crate::session::keyboard::{KeyboardWanted, run_keyboard_capture_session};

const REPROG_INDEX: u8 = 2;
/// The `0x1b04` capability flag that marks a control divertable.
const DIVERTABLE: u8 = 0x20;
/// The `0x1b04` additional capability flag that marks a control able to
/// report raw XY.
const RAW_XY: u8 = 0x01;
/// The `setCidReporting` flag that diverts a control.
const DIVERT: u8 = 0x01;

/// Where the device sends a report it makes on its own.
type Wire = Arc<OnceLock<mpsc::UnboundedSender<Vec<u8>>>>;

/// What the user does with a control: each report the device sends for it,
/// after the pause before it.
type Touch = Vec<(Duration, Vec<u8>)>;

/// A device with `controls` on `0x1b04` at [`REPROG_INDEX`], all divertable
/// with raw XY and native, and no other feature. `setCidReporting` writes are
/// echoed.
///
/// The write that diverts `controls[0]` is followed on the wire by `touch`
/// of that control, given the device index and the control: the user
/// touched it the instant the firmware applied the divert, and arming
/// reaches `controls[1]` only once the touch is over.
fn device(
    controls: [u16; 2],
    wire: Wire,
    touch: fn(u8, u16) -> Touch,
) -> (ScriptedRawHidChannel, ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        // A feature id for the root lookup, a control id for reporting.
        let id = u16::from_be_bytes([request[4], request[5]]);
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) if id == reprog_controls::FEATURE_ID => response[4] = REPROG_INDEX,
            (0, 0) => {}
            (REPROG_INDEX, 0) => response[4] = 2,
            (REPROG_INDEX, 1) => {
                let cid = controls[usize::from(request[4])];
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = DIVERTABLE;
                response[12] = RAW_XY;
            }
            (REPROG_INDEX, 2) => response[4..6].copy_from_slice(&id.to_be_bytes()),
            (REPROG_INDEX, 3) if id == controls[0] && request[6] & DIVERT != 0 => {
                let wire = wire.get().expect("the wire is connected before arming");
                wire.send(request.to_vec()).expect("the channel is open");
                // The channel's read thread hears each report as it is sent,
                // so blocking here spaces their arrivals.
                for (pause, report) in touch(request[1], id) {
                    std::thread::sleep(pause);
                    wire.send(report).expect("the channel is open");
                }
                return None;
            }
            (REPROG_INDEX, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

/// A `divertedButtonsEvent` from `device_index`: `held` are the diverted
/// controls down now.
fn diverted_buttons(device_index: u8, held: &[u16]) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, device_index, REPROG_INDEX, 0x00]);
    for (slot, cid) in report[4..].as_chunks_mut::<2>().0.iter_mut().zip(held) {
        *slot = cid.to_be_bytes();
    }
    report
}

/// A `divertedRawXYEvent` from `device_index`.
fn raw_xy(device_index: u8, dx: i16, dy: i16) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, device_index, REPROG_INDEX, 0x10]);
    report[4..6].copy_from_slice(&dx.to_be_bytes());
    report[6..8].copy_from_slice(&dy.to_be_bytes());
    report
}

/// A press and a release, back to back.
fn click(device_index: u8, cid: u16) -> Touch {
    vec![
        (Duration::ZERO, diverted_buttons(device_index, &[cid])),
        (Duration::ZERO, diverted_buttons(device_index, &[])),
    ]
}

/// The `setCidReporting` writes in `reports` that divert a control, by
/// control id, in the order written.
fn diverted(reports: &[Vec<u8>]) -> Vec<u16> {
    reports
        .iter()
        .filter(|report| report[2] == REPROG_INDEX && report[3] >> 4 == 3)
        .filter(|report| report[6] & DIVERT != 0)
        .map(|report| u16::from_be_bytes([report[4], report[5]]))
        .collect()
}

enum Capture {
    Gesture(CaptureSpec),
    Keyboard(KeyboardWanted),
}

/// Run `capture` on a [`device`] over `controls` whose first control the user
/// touches while arming, until the session reports `button` released, then
/// stop it. Returns everything it sent until then and the reports it wrote.
async fn touch_while_arming(
    controls: [u16; 2],
    touch: fn(u8, u16) -> Touch,
    capture: Capture,
    button: ButtonId,
) -> (Vec<CapturedInput>, Vec<Vec<u8>>) {
    let wire = Wire::default();
    let (raw, handle) = device(controls, Arc::clone(&wire), touch);
    wire.set(raw.incoming_sender())
        .expect("the wire is connected once");
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("capture-node".to_owned()),
        [route.clone()],
        scripted_channel(raw).await,
    );
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let session = tokio::spawn(async move {
        let (_signal, device_io) = device_io_channel();
        let host = CaptureHost {
            sink,
            shutdown: shutdown_rx,
            channel_slot: Arc::new(RwLock::new(None)),
            registry: &registry,
            device_io,
        };
        match capture {
            Capture::Gesture(spec) => run_capture_session(route, spec, host).await,
            Capture::Keyboard(wanted) => {
                run_keyboard_capture_session(route, watch::channel(wanted).1, host).await
            }
        }
    });

    let mut heard = Vec::new();
    let released = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(input) = inputs.recv().await {
            heard.push(input);
            if input == CapturedInput::ButtonUp(button) {
                return;
            }
        }
    })
    .await;
    shutdown
        .send(CaptureSessionStop::Shutdown)
        .expect("the session is still running");
    let outcome: Result<CaptureSessionOutcome, CaptureSessionFailure> =
        tokio::time::timeout(Duration::from_secs(2), session)
            .await
            .expect("the session stops")
            .expect("the session task does not panic");
    assert!(
        matches!(outcome, Ok(CaptureSessionOutcome::Restored)),
        "the session hands its controls back"
    );
    assert!(
        released.is_ok(),
        "the touch while arming reached the sink: {heard:?}"
    );
    (heard, handle.written_reports())
}

#[tokio::test]
async fn a_button_pressed_right_after_its_divert_reaches_the_sink() {
    const MIDDLE: u16 = 0x0052;
    const BACK: u16 = 0x0053;
    let spec = CaptureSpec {
        divert_buttons: vec![(MIDDLE, ButtonId::MiddleClick), (BACK, ButtonId::Back)],
        ..CaptureSpec::default()
    };
    let (heard, written) = touch_while_arming(
        [MIDDLE, BACK],
        click,
        Capture::Gesture(spec),
        ButtonId::MiddleClick,
    )
    .await;

    assert_eq!(
        diverted(&written),
        [MIDDLE, BACK],
        "the press landed while the session still armed the back button"
    );
    let position = |input| heard.iter().position(|heard| *heard == input);
    let down =
        position(CapturedInput::ButtonDown(ButtonId::MiddleClick)).expect("the press is heard");
    for button in [ButtonId::MiddleClick, ButtonId::Back] {
        assert!(
            position(CapturedInput::ButtonDiverted(button)).is_some_and(|at| at < down),
            "{button:?} is reported diverted before the press: {heard:?}"
        );
    }
    assert!(
        position(CapturedInput::ButtonUp(ButtonId::MiddleClick)).is_some_and(|up| up > down),
        "the release follows the press: {heard:?}"
    );
}

#[tokio::test]
async fn a_key_pressed_right_after_its_divert_reaches_the_sink() {
    const MUTE: u16 = 0x00e7;
    const NEXT: u16 = 0x00e8;
    let wanted = KeyboardWanted {
        controls: BTreeMap::from([
            (MUTE, ButtonId::control(MUTE)),
            (NEXT, ButtonId::control(NEXT)),
        ]),
        grid: BTreeMap::new(),
    };
    let mute = ButtonId::control(MUTE);
    let (heard, written) =
        touch_while_arming([MUTE, NEXT], click, Capture::Keyboard(wanted), mute).await;

    assert_eq!(
        diverted(&written),
        [MUTE, NEXT],
        "the press landed while the session still armed the next key"
    );
    assert_eq!(
        heard,
        [
            CapturedInput::ButtonDown(mute),
            CapturedInput::ButtonUp(mute)
        ]
    );
}

#[tokio::test]
async fn a_swipe_made_while_arming_is_timed_by_its_reports() {
    const MIDDLE: u16 = 0x0052;
    /// Hold the gesture button, wait past the swipe gate, move right, let
    /// go. The first raw-XY sample of a hold is stale and dropped.
    fn swipe(device_index: u8, cid: u16) -> Touch {
        vec![
            (Duration::ZERO, diverted_buttons(device_index, &[cid])),
            (Duration::ZERO, raw_xy(device_index, -3000, 2400)),
            (GESTURE_HOLD_FOR_SWIPE * 2, raw_xy(device_index, 120, 5)),
            (Duration::ZERO, diverted_buttons(device_index, &[])),
        ]
    }
    let gesture = reprog_controls::GESTURE_BUTTON_CID;
    let spec = CaptureSpec {
        divert_gesture_sources: vec![gesture],
        divert_buttons: vec![(MIDDLE, ButtonId::MiddleClick)],
        ..CaptureSpec::default()
    };
    let (heard, written) = touch_while_arming(
        [gesture, MIDDLE],
        swipe,
        Capture::Gesture(spec),
        ButtonId::GestureButton,
    )
    .await;

    assert_eq!(
        diverted(&written),
        [gesture, MIDDLE],
        "the swipe was over before the session armed the middle button"
    );
    let gestures: Vec<_> = heard
        .iter()
        .filter(|input| matches!(input, CapturedInput::Gesture(..)))
        .collect();
    assert_eq!(
        gestures,
        [&CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        )],
        "the hold lasted past the swipe gate, so it is a swipe, not a click: {heard:?}"
    );
}

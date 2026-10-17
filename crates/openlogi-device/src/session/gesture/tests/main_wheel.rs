//! The main wheel's `0x2121` divert: the mode arming writes, what a refusal
//! leaves behind, the movement reports it forwards, and the restore that
//! hands the wheel back.

use hidpp::channel::HidppChannel;
use hidpp::nibble::U4;
use openlogi_core::config::ScrollResolution;

use super::*;
use crate::replay::ReplayChannelHandle;

/// Feature index [`wheel_device`] answers for `0x2121`.
const HIRES: u8 = 3;
/// `getWheelCapabilities` flag: the wheel can invert natively.
const HAS_INVERT: u8 = 0x08;
/// Mode byte 0 bits.
const DIVERTED: u8 = 0x01;
const HIGH_RES: u8 = 0x02;
const INVERTED: u8 = 0x04;
/// Firmware-owned mode bytes (priority, rate) every write must carry back.
const PRIORITY: u8 = 0x05;
const RATE: u8 = 0x07;

/// How the scripted wheel answers its `0x2121` requests.
#[derive(Clone, Copy)]
enum SetReply {
    /// Apply the requested mode and echo it.
    Apply,
    /// Apply it without the high-resolution bit and echo what it kept.
    DropHighRes,
    /// Apply a divert, but answer a hand-back to native by echoing the wheel
    /// still diverted.
    IgnoreHandBack,
    /// Answer every write with `InvalidArgument`, changing nothing.
    Refuse,
    /// Answer every request, the capability read included, with
    /// `InvalidArgument`.
    RefuseReads,
    /// Fail every `setWheelMode` at the transport, before the wheel sees it.
    FailWrites,
}

/// Whether `report` is a `setWheelMode` request.
fn is_mode_write(report: &[u8]) -> bool {
    report[2] == HIRES && report[3] >> 4 == 2
}

/// Answer `request` with the HID++ 2.0 `InvalidArgument` error.
fn invalid_argument(request: &[u8], response: &mut [u8]) {
    response[2] = 0xff;
    response[3] = request[2];
    response[4] = request[3];
    response[5] = 0x02;
}

/// A mouse whose only capturable feature is a `0x2121` wheel with multiplier
/// 8, `flags` capabilities and mode byte 0 starting at `mode`. The returned
/// cell is the wheel's live mode.
fn wheel_device(
    flags: u8,
    mode: u8,
    reply: SetReply,
) -> (
    ScriptedRawHidChannel,
    ReplayChannelHandle,
    Arc<Mutex<[u8; 3]>>,
) {
    let state = Arc::new(Mutex::new([mode, PRIORITY, RATE]));
    let wheel = Arc::clone(&state);
    let responder = move |request: &[u8]| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        let mut mode = wheel.lock().unwrap();
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x2121 {
                    response[4] = HIRES;
                }
            }
            (HIRES, _) if matches!(reply, SetReply::RefuseReads) => {
                invalid_argument(request, &mut response);
            }
            (HIRES, 0) => {
                response[4] = 8;
                response[5] = flags;
            }
            (HIRES, 1) => response[4..7].copy_from_slice(&*mode),
            (HIRES, 2) => match reply {
                SetReply::IgnoreHandBack if request[4] & DIVERTED == 0 => {
                    response[4..7].copy_from_slice(&*mode);
                }
                SetReply::Apply | SetReply::IgnoreHandBack => {
                    mode.copy_from_slice(&request[4..7]);
                    response[4..7].copy_from_slice(&*mode);
                }
                SetReply::DropHighRes => {
                    mode.copy_from_slice(&request[4..7]);
                    mode[0] &= !HIGH_RES;
                    response[4..7].copy_from_slice(&*mode);
                }
                SetReply::Refuse | SetReply::RefuseReads => {
                    invalid_argument(request, &mut response);
                }
                SetReply::FailWrites => unreachable!("a failed write never reaches the wheel"),
            },
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    };
    let (raw, handle) = if matches!(reply, SetReply::FailWrites) {
        ScriptedRawHidChannel::with_failing_writes(responder, is_mode_write)
    } else {
        ScriptedRawHidChannel::with_dynamic_responder(responder)
    };
    (raw, handle, state)
}

/// Every `setWheelMode` request's three mode bytes.
fn mode_writes(handle: &ReplayChannelHandle) -> Vec<[u8; 3]> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| is_mode_write(report))
        .map(|report| [report[4], report[5], report[6]])
        .collect()
}

fn spec(resolution: Option<ScrollResolution>, inverted: Option<bool>) -> CaptureSpec {
    CaptureSpec {
        divert_main_wheel: Some(MainWheelDivert {
            resolution,
            inverted,
        }),
        ..CaptureSpec::default()
    }
}

struct Armed {
    result: Result<(), CaptureError>,
    controls: ArmedControls,
    channel: Arc<HidppChannel>,
    handle: ReplayChannelHandle,
    mode: Arc<Mutex<[u8; 3]>>,
}

async fn arm(flags: u8, mode: u8, reply: SetReply, spec: &CaptureSpec) -> Armed {
    let (raw, handle, mode) = wheel_device(flags, mode, reply);
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    let result = arm_controls_into(&device, &channel, 0xff, spec, &mut armed).await;
    Armed {
        result,
        controls: armed,
        channel,
        handle,
        mode,
    }
}

#[tokio::test]
async fn arming_diverts_the_wheel_in_the_configured_mode() {
    let armed = arm(
        HAS_INVERT,
        0,
        SetReply::Apply,
        &spec(Some(ScrollResolution::High), Some(true)),
    )
    .await;
    armed.result.unwrap();
    // Firmware-owned priority and rate go back as they were read.
    assert_eq!(
        mode_writes(&armed.handle),
        [[DIVERTED | HIGH_RES | INVERTED, PRIORITY, RATE]]
    );
    let wheel = armed
        .controls
        .main_wheel
        .as_ref()
        .expect("the wheel is armed");
    assert_eq!(wheel.per_notch.get(), 8);
}

/// Unset fields keep the wheel's own values, and a wheel that cannot invert
/// natively keeps its flag whatever the configuration says.
#[tokio::test]
async fn arming_keeps_what_the_configuration_or_the_wheel_leaves_alone() {
    let armed = arm(0, HIGH_RES, SetReply::Apply, &spec(None, Some(true))).await;
    armed.result.unwrap();
    assert_eq!(
        mode_writes(&armed.handle),
        [[DIVERTED | HIGH_RES, PRIORITY, RATE]]
    );
}

#[tokio::test]
async fn a_wheel_already_diverted_in_that_mode_is_not_written_but_still_owned() {
    let armed = arm(
        0,
        DIVERTED | HIGH_RES,
        SetReply::Apply,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    armed.result.unwrap();
    assert_eq!(mode_writes(&armed.handle), Vec::<[u8; 3]>::new());
    assert!(armed.controls.main_wheel.is_some());
}

/// A diverted wheel that differs only in its native inversion is written:
/// the inversion is one of the fields the divert sets.
#[tokio::test]
async fn a_wheel_diverted_with_another_inversion_is_written() {
    let armed = arm(
        HAS_INVERT,
        DIVERTED,
        SetReply::Apply,
        &spec(None, Some(true)),
    )
    .await;
    armed.result.unwrap();
    assert_eq!(
        mode_writes(&armed.handle),
        [[DIVERTED | INVERTED, PRIORITY, RATE]]
    );
}

#[tokio::test]
async fn without_a_requested_divert_the_wheel_is_not_touched() {
    let armed = arm(0, 0, SetReply::Apply, &CaptureSpec::default()).await;
    armed.result.unwrap();
    assert!(armed.controls.main_wheel.is_none());
    assert!(
        armed
            .handle
            .written_reports()
            .iter()
            .all(|report| report[2] != HIRES),
        "the wheel is neither read nor written"
    );
}

/// A wheel that refuses the divert keeps scrolling natively, and the session
/// arms without it rather than failing every other capture.
#[tokio::test]
async fn a_refused_divert_leaves_the_wheel_native_and_unowned() {
    let armed = arm(
        0,
        0,
        SetReply::Refuse,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    armed.result.unwrap();
    assert!(armed.controls.main_wheel.is_none());
    // The refused request, and no hand-back: the wheel never left native.
    assert_eq!(
        mode_writes(&armed.handle),
        [[DIVERTED | HIGH_RES, PRIORITY, RATE]]
    );
    assert_eq!(*armed.mode.lock().unwrap(), [0, PRIORITY, RATE]);
}

/// An echo showing another mode means the wheel did something else: it is
/// handed back to native before arming goes on without it.
#[tokio::test]
async fn a_divert_echoing_another_mode_is_handed_back() {
    let armed = arm(
        0,
        0,
        SetReply::DropHighRes,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    armed.result.unwrap();
    assert!(armed.controls.main_wheel.is_none());
    assert_eq!(
        mode_writes(&armed.handle),
        [[DIVERTED | HIGH_RES, PRIORITY, RATE], [0, PRIORITY, RATE]]
    );
    assert_eq!(*armed.mode.lock().unwrap(), [0, PRIORITY, RATE]);
}

/// A wheel left diverted that refuses both the new mode and the hand-back
/// fails arming, still owned so the rollback keeps trying to restore it.
#[tokio::test]
async fn a_wheel_refusing_the_hand_back_fails_arming_still_owned() {
    let armed = arm(
        0,
        DIVERTED,
        SetReply::Refuse,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    assert!(armed.result.is_err());
    assert!(armed.controls.main_wheel.is_some());
}

/// A wheel whose capabilities cannot be read is left native, unowned, and
/// the session arms without it.
#[tokio::test]
async fn a_wheel_that_refuses_its_capability_read_is_left_alone() {
    let armed = arm(
        0,
        0,
        SetReply::RefuseReads,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    armed.result.unwrap();
    assert!(armed.controls.main_wheel.is_none());
    assert_eq!(mode_writes(&armed.handle), Vec::<[u8; 3]>::new());
}

/// A transport failure cannot tell whether the wheel took the divert, so
/// arming fails with the wheel still owned, for the rollback to hand back.
#[tokio::test]
async fn a_transport_failure_while_diverting_fails_arming_still_owned() {
    let armed = arm(
        0,
        0,
        SetReply::FailWrites,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    assert!(armed.result.is_err());
    assert!(armed.controls.main_wheel.is_some());
}

fn wheel_event(function: u8, payload_head: &[u8]) -> v20::Message {
    let mut payload = [0; 16];
    payload[..payload_head.len()].copy_from_slice(payload_head);
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: HIRES,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(0),
        },
        payload,
    )
}

/// `0x2121` movement: `high_res` resolution, one period, `vertical` and
/// `horizontal` deltas.
fn movement(high_res: bool, vertical: i16, horizontal: i16) -> v20::Message {
    let [v_high, v_low] = vertical.to_be_bytes();
    let [h_high, h_low] = horizontal.to_be_bytes();
    let head = (u8::from(high_res) << 4) | 1;
    wheel_event(0, &[head, v_high, v_low, h_high, h_low])
}

/// Through the session's handler: a high-resolution report counts the
/// wheel's multiplier per notch, a low-resolution one a single unit. A
/// report that turns nothing vertically, and a ratchet switch, are dropped.
#[tokio::test]
async fn movement_reports_reach_the_dispatcher_as_main_wheel_input() {
    let armed = arm(0, 0, SetReply::Apply, &spec(None, None)).await;
    armed.result.unwrap();
    let capture = GestureCapture::new(armed.controls);
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    handler(&movement(true, 16, 0));
    handler(&movement(false, -1, 0));
    handler(&movement(true, 0, 3));
    handler(&wheel_event(1, &[1]));
    let per_notch = |units| NonZeroU8::new(units).expect("non-zero");
    assert_eq!(
        std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
        [
            CapturedInput::MainWheel {
                delta: 16,
                per_notch: per_notch(8),
            },
            CapturedInput::MainWheel {
                delta: -1,
                per_notch: per_notch(1),
            },
        ]
    );
}

async fn restore_outcome(armed: Armed) -> (CaptureSessionOutcome, ReplayChannelHandle) {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    };
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("mouse-node".to_owned()),
        [route.clone()],
        armed.channel.clone(),
    );
    let shared = SharedChannel::new(armed.channel, route);
    let pending = armed
        .controls
        .into_pending(&shared)
        .expect("a diverted main wheel must be owed a restore");
    let outcome = pending.allow_current_channel().retry(&registry).await;
    (outcome, armed.handle)
}

async fn restore(armed: Armed) -> ReplayChannelHandle {
    let (outcome, handle) = restore_outcome(armed).await;
    assert!(matches!(outcome, CaptureSessionOutcome::Restored));
    handle
}

/// The restore flips only the target, so the resolution and inversion
/// arming wrote stay for native scrolling.
#[tokio::test]
async fn restore_hands_the_wheel_back_with_only_the_target_changed() {
    let armed = arm(
        HAS_INVERT,
        0,
        SetReply::Apply,
        &spec(Some(ScrollResolution::High), Some(true)),
    )
    .await;
    armed.result.as_ref().unwrap();
    let handle = restore(armed).await;
    assert_eq!(
        mode_writes(&handle),
        [
            [DIVERTED | HIGH_RES | INVERTED, PRIORITY, RATE],
            [HIGH_RES | INVERTED, PRIORITY, RATE],
        ]
    );
}

#[tokio::test]
async fn restore_of_a_wheel_already_native_writes_nothing() {
    let armed = arm(0, 0, SetReply::Apply, &spec(None, None)).await;
    armed.result.as_ref().unwrap();
    // The device lost the divert, as a power cycle does.
    *armed.mode.lock().unwrap() = [0, PRIORITY, RATE];
    let handle = restore(armed).await;
    assert_eq!(mode_writes(&handle), [[DIVERTED, PRIORITY, RATE]]);
}

/// A hand-back the wheel does not echo as native is not a restore: it stays
/// owed, so the session keeps retrying it.
#[tokio::test]
async fn a_hand_back_the_wheel_does_not_take_stays_owed() {
    let armed = arm(0, 0, SetReply::IgnoreHandBack, &spec(None, None)).await;
    armed.result.as_ref().unwrap();
    let mode = Arc::clone(&armed.mode);
    let (outcome, handle) = restore_outcome(armed).await;
    assert!(matches!(outcome, CaptureSessionOutcome::RestorePending(_)));
    assert_eq!(
        mode_writes(&handle),
        [[DIVERTED, PRIORITY, RATE], [0, PRIORITY, RATE]]
    );
    assert_eq!(*mode.lock().unwrap(), [DIVERTED, PRIORITY, RATE]);
}

/// A wake re-arm diverts again a wheel that came back native.
#[tokio::test]
async fn rearm_after_wake_diverts_the_wheel_again() {
    let armed = arm(
        0,
        0,
        SetReply::Apply,
        &spec(Some(ScrollResolution::High), None),
    )
    .await;
    armed.result.as_ref().unwrap();
    *armed.mode.lock().unwrap() = [0, PRIORITY, RATE];
    armed.controls.rearm().await;
    assert_eq!(
        mode_writes(&armed.handle),
        [
            [DIVERTED | HIGH_RES, PRIORITY, RATE],
            [DIVERTED | HIGH_RES, PRIORITY, RATE],
        ]
    );
    assert_eq!(
        *armed.mode.lock().unwrap(),
        [DIVERTED | HIGH_RES, PRIORITY, RATE]
    );
}

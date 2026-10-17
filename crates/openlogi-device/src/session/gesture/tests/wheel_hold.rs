//! Wheel-hold controls: raw-wheel arming, the wheel multiplier, delivery, and
//! the restore that hands the wheel back.

use hidpp::channel::HidppChannel;
use hidpp::nibble::U4;

use super::*;

const BACK: u16 = 0x0053;
/// Feature indexes [`wheel_hold_device`] answers for `0x1b04` and `0x2121`.
const REPROG: u8 = 2;
const HIRES: u8 = 3;
/// `0x1b04` `getCidInfo` flags: divertable, and divertable with raw wheel.
const DIVERTABLE: u16 = 0x0020;
const DIVERTABLE_RAW_WHEEL: u16 = 0x0820;

/// A mouse whose only `0x1b04` control is Back with `flags`, plus a `0x2121`
/// wheel with multiplier 8 when `hires`. `setCidReporting` writes are echoed.
fn wheel_hold_device(
    flags: u16,
    hires: bool,
) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => match u16::from_be_bytes([request[4], request[5]]) {
                0x1b04 => response[4] = REPROG,
                0x2121 if hires => response[4] = HIRES,
                _ => {}
            },
            (REPROG, 0) => response[4] = 1,
            (REPROG, 1) => {
                response[4..6].copy_from_slice(&BACK.to_be_bytes());
                let [low, high] = flags.to_le_bytes();
                response[8] = low;
                response[12] = high;
            }
            (REPROG, 2) => response[4..6].copy_from_slice(&BACK.to_be_bytes()),
            (REPROG, 3) => return Some(request.to_vec()),
            (HIRES, 0) if hires => response[4] = 8,
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

/// Every `setCidReporting` as `(cid, flags, flags2)`.
fn reporting_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<(u16, u8, u8)> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == REPROG && report[3] >> 4 == 3)
        .map(|report| {
            (
                u16::from_be_bytes([report[4], report[5]]),
                report[6],
                report[9],
            )
        })
        .collect()
}

fn wheel_hold_spec() -> CaptureSpec {
    CaptureSpec {
        divert_wheel_buttons: vec![(BACK, ButtonId::Back)],
        ..CaptureSpec::default()
    }
}

async fn arm(
    flags: u16,
    hires: bool,
) -> (
    ArmedControls,
    Arc<HidppChannel>,
    crate::replay::ReplayChannelHandle,
) {
    arm_spec(flags, hires, &wheel_hold_spec()).await
}

async fn arm_spec(
    flags: u16,
    hires: bool,
    spec: &CaptureSpec,
) -> (
    ArmedControls,
    Arc<HidppChannel>,
    crate::replay::ReplayChannelHandle,
) {
    let (raw, handle) = wheel_hold_device(flags, hires);
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, spec, &mut armed)
        .await
        .unwrap();
    (armed, channel, handle)
}

#[tokio::test]
async fn a_raw_wheel_control_arms_raw_wheel_and_reads_the_multiplier() {
    for hires in [false, true] {
        let (armed, _channel, handle) = arm(DIVERTABLE_RAW_WHEEL, hires).await;
        // Diverted with raw XY off, and raw wheel on (`0x0c`).
        assert_eq!(
            reporting_writes(&handle),
            [(BACK, 0x23, 0x0c)],
            "hires {hires}"
        );
        assert_eq!(armed.wheel_button_cids, [(BACK, ButtonId::Back)]);
        assert!(armed.button_cids.is_empty());
        assert_eq!(
            armed.hires_per_notch,
            if hires { NonZeroU8::new(8) } else { None }
        );
    }
}

#[tokio::test]
async fn a_control_without_raw_wheel_holds_as_a_plain_click() {
    let (armed, _channel, handle) = arm(DIVERTABLE, true).await;
    assert_eq!(reporting_writes(&handle), [(BACK, 0x23, 0x00)]);
    assert_eq!(armed.button_cids, [(BACK, ButtonId::Back)]);
    assert!(armed.wheel_button_cids.is_empty());
    assert_eq!(armed.hires_per_notch, None);
    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| report[2] != HIRES),
        "no wheel-hold control was armed, so the multiplier is not read"
    );
}

#[tokio::test]
async fn a_non_divertable_control_is_left_alone() {
    let (armed, _channel, handle) = arm(0x0800, true).await;
    assert_eq!(reporting_writes(&handle), []);
    assert!(armed.wheel_button_cids.is_empty() && armed.button_cids.is_empty());
}

/// A control an earlier arming step already owns keeps that owner: arming it
/// again would overwrite its reporting and record a second restore for it.
#[tokio::test]
async fn a_control_armed_for_its_click_is_not_armed_again_for_the_wheel() {
    let spec = CaptureSpec {
        divert_buttons: vec![(BACK, ButtonId::Back)],
        ..wheel_hold_spec()
    };
    let (armed, _channel, handle) = arm_spec(DIVERTABLE_RAW_WHEEL, true, &spec).await;
    assert_eq!(reporting_writes(&handle), [(BACK, 0x23, 0x00)]);
    assert_eq!(armed.button_cids, [(BACK, ButtonId::Back)]);
    assert!(armed.wheel_button_cids.is_empty());
}

fn reprog_event(event: u8, payload_head: &[u8]) -> v20::Message {
    let mut payload = [0; 16];
    payload[..payload_head.len()].copy_from_slice(payload_head);
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: REPROG,
            function_id: U4::from_lo(event),
            software_id: U4::from_lo(0),
        },
        payload,
    )
}

/// Through the session's handler: Back goes down, the wheel turns in high
/// resolution, and Back comes up.
#[tokio::test]
async fn raw_wheel_while_back_is_held_reaches_the_dispatcher_in_order() {
    let (armed, _channel, _handle) = arm(DIVERTABLE_RAW_WHEEL, true).await;
    let capture = GestureCapture::new(armed);
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    handler(&reprog_event(0, &BACK.to_be_bytes()));
    let [high, low] = (-16i16).to_be_bytes();
    // divertedRawWheel: high resolution (bit 4), one period, delta -16.
    handler(&reprog_event(4, &[0x11, high, low]));
    handler(&reprog_event(0, &[0, 0]));
    assert_eq!(
        std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
        [
            CapturedInput::ButtonDown(ButtonId::Back),
            CapturedInput::HeldWheel {
                button: ButtonId::Back,
                delta: -16,
                per_notch: NonZeroU8::new(8).expect("non-zero"),
            },
            CapturedInput::ButtonUp(ButtonId::Back),
        ]
    );
}

/// The restore clears raw wheel (`0x08`) as well as the diversion, so the
/// wheel scrolls natively again after capture ends.
#[tokio::test]
async fn restore_hands_the_wheel_back_to_the_firmware() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    };
    let (armed, channel, handle) = arm(DIVERTABLE_RAW_WHEEL, false).await;
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("mouse-node".to_owned()),
        [route.clone()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel, route);
    let pending = armed
        .into_pending(&shared)
        .expect("a raw-wheel control must be owed a restore");
    assert!(matches!(
        pending.allow_current_channel().retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(
        reporting_writes(&handle),
        [(BACK, 0x23, 0x0c), (BACK, 0x22, 0x08)]
    );
}

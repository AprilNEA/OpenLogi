//! The Gestures2 thumb wheel (gesture 46) on a device without `0x2150`:
//! arming, reports, the wake re-divert and teardown.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use hidpp::feature::CreatableFeature as _;
use hidpp::feature::gestures2::{Gestures2Feature, THUMBWHEEL_GESTURE_ID};
use hidpp::nibble::U4;

use super::*;

/// The `0x6501` index the device sits at when it is armed.
const ARMED_INDEX: u8 = 13;

/// Diversion write bodies (`offset, 0x01, mask, value`) for gesture 46 at
/// diversion index 0: value `mask` diverts it, `0` selects native reporting.
const DIVERT: [u8; 4] = [0, 1, 1, 1];
const NATIVE: [u8; 4] = [0, 1, 1, 0];

/// A mouse without `0x2150` whose `0x6501` sits at `index` (`0` means
/// absent). Gesture 46 is the first divertable descriptor entry and reads as
/// not diverted; diversion writes are echoed.
fn gesture46_responder(
    index: Arc<AtomicU8>,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        let feature = index.load(Ordering::SeqCst);
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                if u16::from_be_bytes([request[4], request[5]]) == Gestures2Feature::ID {
                    response[4] = feature;
                }
            }
            // Descriptor page: entry 0 is gesture 46, enabled and divertable.
            (idx, 0) if idx == feature => {
                response[4..6].copy_from_slice(&[0x83, THUMBWHEEL_GESTURE_ID]);
            }
            // Diversion read: the bit is clear.
            (idx, 3) if idx == feature => {}
            // Diversion write: acknowledged by echoing the request.
            (idx, 4) if idx == feature => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    }
}

/// The body of every diversion write sent to feature `index`, in order.
fn diversion_writes(handle: &crate::replay::ReplayChannelHandle, index: u8) -> Vec<[u8; 4]> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == index && report[3] >> 4 == 4)
        .map(|report| [report[4], report[5], report[6], report[7]])
        .collect()
}

fn is_diversion_write(report: &[u8]) -> bool {
    report[2] == ARMED_INDEX && report[3] >> 4 == 4
}

fn route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    }
}

fn thumbwheel_spec() -> CaptureSpec {
    CaptureSpec {
        capture_thumbwheel: true,
        ..CaptureSpec::default()
    }
}

/// A device of [`gesture46_responder`] armed for thumb-wheel capture with
/// `0x6501` at [`ARMED_INDEX`].
struct ArmedGesture46 {
    armed: ArmedControls,
    shared: SharedChannel,
    registry: ChannelRegistry,
    handle: crate::replay::ReplayChannelHandle,
    /// Where `0x6501` sits now; a test moves it to change the feature table.
    index: Arc<AtomicU8>,
}

async fn arm_gesture46() -> ArmedGesture46 {
    let index = Arc::new(AtomicU8::new(ARMED_INDEX));
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(gesture46_responder(Arc::clone(&index)));
    let channel = scripted_channel(raw).await;
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("mouse-node".to_owned()),
        [route()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel.clone(), route());
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &thumbwheel_spec(), &mut armed)
        .await
        .unwrap();
    ArmedGesture46 {
        armed,
        shared,
        registry,
        handle,
        index,
    }
}

/// Hand `armed` back through the capture restore path.
async fn hand_back(armed: ArmedControls, shared: &SharedChannel, registry: &ChannelRegistry) {
    let pending = armed
        .into_pending(shared)
        .expect("a diverted gesture 46 must be owed a restore");
    assert!(matches!(
        pending.allow_current_channel().retry(registry).await,
        CaptureSessionOutcome::Restored
    ));
}

/// A report on the armed `0x6501` whose payload starts with `head`.
fn gesture46_report(head: [u8; 4]) -> v20::Message {
    let mut payload = [0; 16];
    payload[..4].copy_from_slice(&head);
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: ARMED_INDEX,
            function_id: U4::from_lo(0),
            software_id: U4::from_lo(0),
        },
        payload,
    )
}

fn step_report(step: i8) -> v20::Message {
    let [step] = step.to_be_bytes();
    gesture46_report([0x23, 0x00, 0x03, step])
}

fn scroll(increments: i16) -> CapturedInput {
    CapturedInput::Scroll {
        increments,
        resolution: WheelResolution::UNKNOWN,
    }
}

/// Without `0x2150`, thumb-wheel capture diverts gesture 46 and owns it. Its
/// steps scroll as they are: gesture 46 reports no resolution and no
/// polarity to publish.
#[tokio::test]
async fn a_wheel_without_0x2150_is_captured_through_gesture46() {
    let device = arm_gesture46().await;
    assert!(matches!(device.armed.thumb, Some(ArmedThumb::Gesture46(_))));
    assert_eq!(device.armed.thumbwheel_direction(), None);
    assert_eq!(diversion_writes(&device.handle, ARMED_INDEX), vec![DIVERT]);

    let capture = GestureCapture::new(device.armed);
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    // The roll's start marker carries no step.
    handler(&gesture46_report([0x11, 0x00, 0x00, 0x00]));
    handler(&step_report(2));
    handler(&step_report(-2));
    assert_eq!(
        std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
        vec![scroll(2), scroll(-2)]
    );
}

/// A wake drops volatile diversion, so the session diverts gesture 46 again.
#[tokio::test]
async fn gesture46_is_diverted_again_after_a_wake() {
    let device = arm_gesture46().await;
    device.armed.rearm().await;
    assert_eq!(
        diversion_writes(&device.handle, ARMED_INDEX),
        vec![DIVERT, DIVERT]
    );
}

/// Teardown selects native reporting on the feature the root reports at
/// teardown, not the index found at arming.
#[tokio::test]
async fn restore_finds_gesture46_again_instead_of_reusing_the_arming_index() {
    let device = arm_gesture46().await;
    // The feature table changed since arming, e.g. by a firmware update.
    device.index.store(9, Ordering::SeqCst);
    hand_back(device.armed, &device.shared, &device.registry).await;
    assert_eq!(diversion_writes(&device.handle, 9), vec![NATIVE]);
    assert_eq!(
        diversion_writes(&device.handle, ARMED_INDEX),
        vec![DIVERT],
        "only the arming write goes to the old index"
    );
}

/// A device that no longer reports `0x6501` has nothing to restore.
#[tokio::test]
async fn restore_writes_nothing_when_gestures2_is_gone() {
    let device = arm_gesture46().await;
    device.index.store(0, Ordering::SeqCst);
    hand_back(device.armed, &device.shared, &device.registry).await;
    assert_eq!(diversion_writes(&device.handle, ARMED_INDEX), vec![DIVERT]);
}

/// A transport failure on the divert cannot prove the firmware ignored it, so
/// arming fails with gesture 46 still owned and owed a restore.
#[tokio::test]
async fn a_failed_gesture46_divert_still_owes_a_restore() {
    let index = Arc::new(AtomicU8::new(ARMED_INDEX));
    let (raw, _handle) =
        ScriptedRawHidChannel::with_failing_writes(gesture46_responder(index), is_diversion_write);
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &thumbwheel_spec(), &mut armed)
        .await
        .unwrap_err();
    assert!(
        matches!(armed.thumb, Some(ArmedThumb::Gesture46(_))),
        "the wheel must stay owned"
    );
    let shared = SharedChannel::new(channel, route());
    assert!(armed.into_pending(&shared).is_some());
}

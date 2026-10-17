//! Thumb-wheel reports: rolls, taps, and contact without either.

use hidpp::nibble::U4;

use super::*;
use thumbwheel::RotationStatus;

/// The resolutions an MX Master 4 reports: 20 ratchets natively, 120
/// increments diverted, so one increment is a sixth of a native scroll unit.
const TRACED_RES: thumbwheel::WheelResolution = thumbwheel::WheelResolution {
    native_res: 20,
    diverted_res: 120,
};

fn thumb_event(
    rotation: i16,
    rotation_status: RotationStatus,
    single_tap: bool,
) -> thumbwheel::ThumbwheelEvent {
    thumbwheel::ThumbwheelEvent {
        rotation,
        rotation_status,
        single_tap,
        touch: true,
        proxy: true,
    }
}

/// The wheel's touch sensor flags a tap on the same contact that rolled it, so
/// a rolling report's tap bit is an artifact — forwarding it fired the tap's
/// bound action in the middle of a scroll.
#[test]
fn a_rolling_report_is_a_roll_even_when_it_flags_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(-3, RotationStatus::Active, true),
            RotationStatus::Start,
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: -3,
            resolution: TRACED_RES
        })
    );
}

/// The roll's closing report: the finger lifts, so it carries no rotation of
/// its own while the sensor flags the contact that just rolled the wheel. Only
/// `rotation_status` separates it from a deliberate tap.
#[test]
fn the_release_that_ends_a_roll_is_not_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, RotationStatus::Stop, true),
            RotationStatus::Active,
            TRACED_RES
        ),
        None
    );
}

/// A firmware that drops a roll's `Stop` reports the release as `Inactive`,
/// tap bit and all. Following a roll still open, that report ends the roll.
#[test]
fn a_release_after_a_roll_left_open_is_not_a_tap() {
    for previous in [RotationStatus::Start, RotationStatus::Active] {
        assert_eq!(
            thumbwheel_input(
                thumb_event(0, RotationStatus::Inactive, true),
                previous,
                TRACED_RES
            ),
            None,
            "after {previous:?}"
        );
    }
}

#[test]
fn a_tap_on_a_settled_wheel_is_a_tap() {
    for previous in [RotationStatus::Inactive, RotationStatus::Stop] {
        assert_eq!(
            thumbwheel_input(
                thumb_event(0, RotationStatus::Inactive, true),
                previous,
                TRACED_RES
            ),
            Some(CapturedInput::ButtonPulse(ButtonId::Thumbwheel)),
            "after {previous:?}"
        );
    }
}

/// A wheel whose firmware leaves byte 4 at zero still has its own rotation to
/// go on, so the roll is recognised without the status field.
#[test]
fn rotation_alone_still_marks_a_roll() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(4, RotationStatus::Inactive, true),
            RotationStatus::Inactive,
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: 4,
            resolution: TRACED_RES
        })
    );
}

/// Touch and proximity alone carry no input: the wheel reports them whenever a
/// thumb rests near it.
#[test]
fn contact_without_rotation_or_a_tap_carries_no_input() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, RotationStatus::Inactive, false),
            RotationStatus::Inactive,
            TRACED_RES
        ),
        None
    );
}

/// A gesture capture with the thumb wheel of
/// [`thumbwheel_device`](super::session::thumbwheel_device) armed: device
/// `0xff`, `0x2150` at index 3, resolutions as [`TRACED_RES`].
async fn armed_thumbwheel_capture() -> GestureCapture {
    let (raw, _handle) = super::session::thumbwheel_device([0x00, 0x00]);
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        capture_thumbwheel: true,
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    GestureCapture::new(armed)
}

/// A `thumbwheelEvent` from the wheel [`armed_thumbwheel_capture`] arms, with
/// the touch bit set as it is while a thumb is on the wheel.
fn wheel_report(rotation: i16, status: u8, single_tap: bool) -> v20::Message {
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&rotation.to_be_bytes());
    payload[4] = status;
    payload[5] = if single_tap { 0x0a } else { 0x02 };
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: 3,
            function_id: U4::from_lo(0),
            software_id: U4::from_lo(0),
        },
        payload,
    )
}

fn received(inputs: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
    std::iter::from_fn(|| inputs.try_recv().ok()).collect()
}

fn scroll(increments: i16) -> CapturedInput {
    CapturedInput::Scroll {
        increments,
        resolution: TRACED_RES,
    }
}

/// Through the session's handler: a roll whose `Stop` never arrives ends at
/// the release, and the next tap on the settled wheel still fires.
#[tokio::test]
async fn a_roll_that_never_reports_its_stop_ends_without_a_tap() {
    let capture = armed_thumbwheel_capture().await;
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    handler(&wheel_report(2, 1, true));
    handler(&wheel_report(3, 2, true));
    // The release, reported as `Inactive` in place of the dropped `Stop`.
    handler(&wheel_report(0, 0, true));
    handler(&wheel_report(0, 0, true));
    assert_eq!(
        received(&mut inputs),
        vec![
            scroll(2),
            scroll(3),
            CapturedInput::ButtonPulse(ButtonId::Thumbwheel)
        ]
    );
}

/// A device power cycle mid-roll ends that roll: the first tap after the
/// reconnect is the user's.
#[tokio::test]
async fn a_reconnect_forgets_a_roll_left_open() {
    let capture = armed_thumbwheel_capture().await;
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);
    handler(&wheel_report(3, 2, true));
    capture.reset_input_state();
    handler(&wheel_report(0, 0, true));
    assert_eq!(
        received(&mut inputs),
        vec![scroll(3), CapturedInput::ButtonPulse(ButtonId::Thumbwheel)]
    );
}

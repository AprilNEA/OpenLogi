use super::*;

use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hidpp::channel::{LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel};

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

fn thumbwheel_event(rotation: i16, single_tap: bool) -> ThumbwheelEvent {
    ThumbwheelEvent {
        rotation,
        single_tap,
        touch: single_tap,
        proxy: false,
    }
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Click))
    );
    assert!(
        rx.try_recv().is_err(),
        "a quick tap emits exactly one click"
    );
}

#[test]
fn a_held_gesture_commits_a_swipe_and_does_not_also_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(&mut acc, release(), &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed swipe must not also click on release"
    );
}

#[test]
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert!(rx.try_recv().is_err(), "a held DPI button presses once");
}

#[test]
fn a_dpi_button_re_presses_after_a_release() {
    // Rising-edge detection must re-arm: press → release → press is two
    // distinct presses. The release (a frame without the CID) is what resets
    // the edge; without it a re-press would be swallowed as "still held".
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, up, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle)),
        "a release re-arms the rising edge"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn rotation_only_capture_suppresses_taps_but_forwards_rotation() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    forward_thumbwheel_event(
        thumbwheel_event(7, true),
        ThumbwheelCaptureMode::DivertedRotation,
        &tx,
    );

    assert_eq!(rx.try_recv(), Ok(CapturedInput::Scroll(7)));
    assert!(
        rx.try_recv().is_err(),
        "rotation-only capture must not deliver the tap"
    );
}

#[test]
fn rotation_and_tap_capture_forwards_each_input_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    forward_thumbwheel_event(
        thumbwheel_event(-4, true),
        ThumbwheelCaptureMode::DivertedRotationAndTap,
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Thumbwheel))
    );
    assert_eq!(rx.try_recv(), Ok(CapturedInput::Scroll(-4)));
    assert!(
        rx.try_recv().is_err(),
        "one report must emit exactly one tap and one rotation"
    );
}

#[test]
fn zero_motion_without_a_tap_emits_nothing() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    forward_thumbwheel_event(
        thumbwheel_event(0, false),
        ThumbwheelCaptureMode::DivertedRotationAndTap,
        &tx,
    );

    assert!(rx.try_recv().is_err());
}

#[test]
fn native_mode_ignores_a_decoded_thumbwheel_report() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    forward_thumbwheel_event(
        thumbwheel_event(3, true),
        ThumbwheelCaptureMode::Native,
        &tx,
    );

    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn partial_arm_failure_restores_every_diverted_control() {
    let (raw, mut written_reports) = ArmRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let channel = Arc::new(channel);

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        arm_controls(
            &channel,
            1,
            ThumbwheelCaptureMode::DivertedRotationAndTap,
            true,
        ),
    )
    .await;
    let Ok(result) = result else {
        panic!("scripted arm sequence timed out");
    };
    assert!(matches!(result, Err(GestureError::Hidpp(_))));

    let reports: Vec<_> = std::iter::from_fn(|| written_reports.try_recv().ok()).collect();
    let changes: Vec<_> = reports
        .iter()
        .filter_map(|report| {
            (report.len() == LONG_REPORT_LENGTH
                && report[0] == LONG_REPORT_ID
                && report[2] == 5
                && report[3] >> 4 == 3)
                .then(|| {
                    (
                        u16::from_be_bytes([report[4], report[5]]),
                        report[6] & 0x01 != 0,
                        report[6] & 0x10 != 0,
                    )
                })
        })
        .collect();
    assert_eq!(
        changes,
        vec![
            (reprog_controls::GESTURE_BUTTON_CID, true, true),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[0], true, false),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[1], true, false),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[2], true, false),
            (reprog_controls::GESTURE_BUTTON_CID, false, false),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[0], false, false),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[1], false, false),
            (reprog_controls::DPI_MODE_SHIFT_CIDS[2], false, false),
        ],
        "the retained reprog accessor must hand back the gesture and every DPI CID"
    );

    let thumbwheel_modes: Vec<_> = reports
        .iter()
        .filter(|report| {
            report.len() == LONG_REPORT_LENGTH
                && report[0] == LONG_REPORT_ID
                && report[2] == 6
                && report[3] >> 4 == 2
        })
        .map(|report| report[4])
        .collect();
    assert_eq!(
        thumbwheel_modes,
        vec![1, 0],
        "a possibly-applied failing thumbwheel arm is also handed back"
    );
}

struct ArmRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written_tx: mpsc::UnboundedSender<Vec<u8>>,
    fail_thumbwheel_arm: AtomicBool,
}

impl ArmRawHidChannel {
    fn new() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (written_tx, written_rx) = mpsc::unbounded_channel();
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written_tx,
                fail_thumbwheel_arm: AtomicBool::new(true),
            },
            written_rx,
        )
    }

    fn response_for(report: &[u8]) -> Vec<u8> {
        let mut response = report.to_vec();
        let feature = report[2];
        let function = report[3] >> 4;
        match (feature, function) {
            // Root ping: advertise HID++ 2.0.
            (0, 1) => response[4..7].copy_from_slice(&[2, 0, 0]),
            // Root getFeature: expose reprog controls and thumbwheel.
            (0, 0) => {
                response[4..7].fill(0);
                response[4] = match u16::from_be_bytes([report[4], report[5]]) {
                    reprog_controls::FEATURE_ID => 5,
                    thumbwheel::FEATURE_ID => 6,
                    _ => 0,
                };
            }
            // Reprog getCount: gesture plus every supported DPI/ModeShift CID.
            (5, 0) => response[4] = 4,
            // Reprog getCidInfo.
            (5, 1) => {
                response[4..].fill(0);
                let index = usize::from(report[4]);
                let cids = [
                    reprog_controls::GESTURE_BUTTON_CID,
                    reprog_controls::DPI_MODE_SHIFT_CIDS[0],
                    reprog_controls::DPI_MODE_SHIFT_CIDS[1],
                    reprog_controls::DPI_MODE_SHIFT_CIDS[2],
                ];
                let cid = cids[index].to_be_bytes();
                response[4..6].copy_from_slice(&cid);
                response[8] = 0x20; // DIVERTABLE
                if index == 0 {
                    response[12] = 0x01; // RAW_XY (the high flags byte)
                }
            }
            // Thumbwheel getInfo: report single-tap support.
            (6, 0) => {
                response[4..].fill(0);
                response[9] = 0x08;
            }
            // setCidReporting / setThumbwheelReporting responses echo the request.
            (5, 3) | (6, 2) => {}
            _ => {}
        }
        response
    }
}

#[hidpp::async_trait]
impl RawHidChannel for ArmRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc548
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        let report = src.to_vec();
        if self.written_tx.send(report.clone()).is_err() {
            return Err(arm_mock_error("written-report receiver closed"));
        }
        let fails_after_reprog_arming = report.len() == LONG_REPORT_LENGTH
            && report[0] == LONG_REPORT_ID
            && report[2] == 6
            && report[3] >> 4 == 2
            && report[4] == 1
            && self.fail_thumbwheel_arm.swap(false, Ordering::SeqCst);
        if fails_after_reprog_arming {
            return Err(arm_mock_error("injected thumbwheel arm failure"));
        }
        if self.incoming_tx.send(Self::response_for(&report)).is_err() {
            return Err(arm_mock_error("incoming-report receiver closed"));
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return std::future::pending().await;
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>> {
        Err(arm_mock_error("descriptor should not be requested"))
    }
}

fn arm_mock_error(message: &str) -> Box<dyn Error + Sync + Send> {
    Box::new(io::Error::other(message.to_string()))
}

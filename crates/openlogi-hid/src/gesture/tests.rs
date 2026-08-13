use super::*;

use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hidpp::channel::{LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel};

const GESTURE: &[u16] = &[reprog_controls::GESTURE_BUTTON_CID];
const PANEL: &[u16] = &[reprog_controls::HAPTIC_PANEL_CID];
const BOTH: &[u16] = &[
    reprog_controls::GESTURE_BUTTON_CID,
    reprog_controls::HAPTIC_PANEL_CID,
];

#[test]
fn reporting_restore_preserves_every_mutable_field() {
    let remap = reprog_controls::ControlId(0x0053);
    let original = reprog_controls::CidReporting {
        cid: reprog_controls::ControlId(reprog_controls::GESTURE_BUTTON_CID),
        diverted: true,
        persistently_diverted: true,
        force_raw_xy: true,
        raw_xy: false,
        remap: Some(remap),
        analytics_key_events: true,
        raw_wheel: true,
    };

    assert_eq!(
        reporting_change(original),
        reprog_controls::CidReportingChange {
            diverted: Some(true),
            persistently_diverted: Some(true),
            force_raw_xy: Some(true),
            raw_xy: Some(false),
            remap: Some(remap),
            analytics_key_events: Some(true),
            raw_wheel: Some(true),
        }
    );
}

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn panel_press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::HAPTIC_PANEL_CID, 0, 0, 0])
}

fn both_press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([
        reprog_controls::GESTURE_BUTTON_CID,
        reprog_controls::HAPTIC_PANEL_CID,
        0,
        0,
    ])
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
fn a_still_held_second_source_takes_over_when_the_holder_releases() {
    // Both sources diverted: press the gesture button, add the panel, release
    // the gesture button (click — no swipe committed), and the still-held
    // panel must become the new holder so its subsequent swipe dispatches —
    // not be swallowed until its own release.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, both_press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, panel_press(), BOTH, &[], &[], &tx);
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "the released holder still clicks"
    );

    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        )),
        "the taken-over hold dispatches through the panel's own map"
    );

    handle_reprog(&mut acc, release(), BOTH, &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed takeover swipe must not also click on release"
    );
}

#[test]
fn raw_xy_during_a_two_source_overlap_is_dropped_not_misattributed() {
    // Raw-XY reports carry no source attribution: while BOTH sources are held,
    // motion must not commit through the first holder's map (the reports could
    // as well be the other control's). Motion resumes once the overlap ends.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(&mut acc, both_press(), BOTH, &[], &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "ambiguous overlap motion must not commit a swipe"
    );

    // The panel lifts; the surviving hold accumulates again.
    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        )),
        "the original hold resumes once the overlap ends"
    );
}

#[test]
fn a_same_report_swap_to_the_panel_still_discards_its_contact_jump() {
    // Holder release and panel press arriving in ONE report: the takeover must
    // treat the panel as freshly touched, so its first raw-XY sample (the
    // absolute contact jump) is discarded before the accumulator sees it.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, panel_press(), BOTH, &[], &[], &tx);
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "the swapped-out holder still clicks"
    );

    acc.swipe.backdate_hold_for_test();
    // The contact jump — leftward, far past every threshold — must be dropped.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "the panel's contact jump must not commit a swipe"
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        ))
    );
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        GESTURE,
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        ))
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

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        GESTURE,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        ))
    );

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed swipe must not also click on release"
    );
}

#[test]
fn the_haptic_panel_gestures_when_diverted_for_gestures() {
    // On MX Master 4 the panel (CID 0x01a0) can gesture: its press begins a
    // hold, its contact jump is discarded, and the raw-XY that follows
    // commits a swipe.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), PANEL, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    // The panel's contact jump, discarded before the accumulator sees it.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        PANEL,
        &[],
        &[],
        &tx,
    );
    // The real swipe that follows.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 5, dy: -120 },
        PANEL,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Up
        ))
    );

    handle_reprog(&mut acc, release(), PANEL, &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed panel swipe must not also click on release"
    );
}

#[test]
fn a_quick_panel_tap_is_a_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), PANEL, &[], &[], &tx);
    handle_reprog(&mut acc, release(), PANEL, &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Click
        ))
    );
    assert!(
        rx.try_recv().is_err(),
        "a panel tap emits exactly one click"
    );
}

#[test]
fn the_panels_first_raw_xy_sample_after_contact_is_discarded() {
    // Real-hardware probe finding: the panel's first raw-XY sample after
    // contact is a large position jump (up to thousands of units), not a
    // relative delta. Un-discarded it would instantly commit a bogus swipe.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), PANEL, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    // The contact jump — leftward, far past every threshold.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        PANEL,
        &[],
        &[],
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "the contact jump must not commit a swipe"
    );
    // The real swipe starts from a clean accumulator: had the jump been
    // summed, this rightward travel could never commit Right.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        PANEL,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        ))
    );
}

#[test]
fn the_dedicated_buttons_first_sample_is_not_discarded() {
    // The discard is a panel quirk: the dedicated button's raw-XY stream is
    // relative from the first sample, which must keep committing as-is.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        GESTURE,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        )),
        "the dedicated button's very first sample still counts"
    );
}

#[test]
fn an_undiverted_gesture_source_does_not_gesture() {
    // Only the panel is diverted for gestures; a dedicated-button press must
    // not begin a hold, emit a click, or feed the swipe accumulator — the two
    // sources are distinct physical controls.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), PANEL, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        PANEL,
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), PANEL, &[], &[], &tx);

    assert!(
        rx.try_recv().is_err(),
        "a non-owner source must neither gesture nor click"
    );
}

#[test]
fn a_plain_diverted_gesture_button_presses_without_gesturing() {
    // A gesture button diverted as a plain button (not in gesture mode; its
    // single binding needs delivery) must dispatch as a button press only —
    // the swipe accumulator belongs to the raw-XY gesture diverts and must
    // not also emit a gesture click on release.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let buttons = [(reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton)];

    handle_reprog(&mut acc, press(), &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, release(), &[], &[], &buttons, &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::GestureButton, None))
    );
    assert!(
        rx.try_recv().is_err(),
        "a plain-diverted gesture button must not also emit a gesture click"
    );
}

#[test]
fn a_plain_diverted_haptic_panel_presses_as_its_own_button() {
    // A single action bound to the panel (which is not in gesture mode) is
    // delivered as ButtonId::HapticPanel — its own control, never conflated
    // with the dedicated gesture button.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let buttons = [(reprog_controls::HAPTIC_PANEL_CID, ButtonId::HapticPanel)];

    handle_reprog(&mut acc, panel_press(), &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, release(), &[], &[], &buttons, &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::HapticPanel, None))
    );
    assert!(
        rx.try_recv().is_err(),
        "a plain-diverted panel must not also emit a gesture click"
    );
}

#[test]
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

    handle_reprog(&mut acc, down, GESTURE, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, GESTURE, &[dpi], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None))
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

    handle_reprog(&mut acc, down, GESTURE, &[dpi], &[], &tx);
    handle_reprog(&mut acc, up, GESTURE, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, GESTURE, &[dpi], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None)),
        "a release re-arms the rising edge"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn diverted_report_forwards_tap_and_rotation_for_live_agent_policy() {
    let (tx, mut rx) = mpsc::unbounded_channel();

    forward_thumbwheel_event(thumbwheel_event(-4, true), &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Thumbwheel, None))
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

    forward_thumbwheel_event(thumbwheel_event(0, false), &tx);

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
            &CaptureSpec {
                thumbwheel_mode: ThumbwheelCaptureMode::Diverted,
                divert_gesture_sources: vec![reprog_controls::GESTURE_BUTTON_CID],
                divert_buttons: Vec::new(),
            },
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

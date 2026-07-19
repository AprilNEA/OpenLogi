use super::*;

/// The gesture sources of a device exposing both the dedicated button and the
/// MX Master 4 haptic panel — the widest arming `arm_controls` can produce.
const BOTH_SOURCES: [u16; 2] = [
    reprog_controls::GESTURE_BUTTON_CID,
    reprog_controls::HAPTIC_PANEL_CID,
];

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn panel_press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::HAPTIC_PANEL_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let sources = [reprog_controls::GESTURE_BUTTON_CID];

    handle_reprog(&mut acc, press(), &sources, &[], &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &sources,
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &sources, &[], &[], &tx);

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
    let sources = [reprog_controls::GESTURE_BUTTON_CID];

    handle_reprog(&mut acc, press(), &sources, &[], &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &sources,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(&mut acc, release(), &sources, &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed swipe must not also click on release"
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn the_haptic_panel_gestures_like_the_dedicated_button() {
    // On MX Master 4 the gesture role is delivered by the haptic panel (CID
    // 0x01a0): its press must begin a hold and its raw-XY must commit a swipe,
    // even when the dedicated button's CID is also armed.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), &BOTH_SOURCES, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    // The panel's contact jump, discarded before the accumulator sees it.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        &BOTH_SOURCES,
        &[],
        &[],
        &tx,
    );
    // The real swipe that follows.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 5, dy: -120 },
        &BOTH_SOURCES,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Up))
    );

    handle_reprog(&mut acc, release(), &BOTH_SOURCES, &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed panel swipe must not also click on release"
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn a_quick_panel_tap_is_a_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), &BOTH_SOURCES, &[], &[], &tx);
    handle_reprog(&mut acc, release(), &BOTH_SOURCES, &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Click))
    );
    assert!(
        rx.try_recv().is_err(),
        "a panel tap emits exactly one click"
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn the_panels_first_raw_xy_sample_after_contact_is_discarded() {
    // Real-hardware probe finding: the panel's first raw-XY sample after
    // contact is a large position jump (up to thousands of units), not a
    // relative delta. Un-discarded it would instantly commit a bogus swipe.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), &BOTH_SOURCES, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    // The contact jump — leftward, far past every threshold.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        &BOTH_SOURCES,
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
        &BOTH_SOURCES,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn the_dedicated_buttons_first_sample_is_not_discarded() {
    // The discard is a panel quirk: the dedicated button's raw-XY stream is
    // relative from the first sample, which must keep committing as-is.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &BOTH_SOURCES, &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &BOTH_SOURCES,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right)),
        "the dedicated button's very first sample still counts"
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn a_plain_diverted_haptic_panel_presses_as_the_gesture_button() {
    // With gestures off and a single binding on [`ButtonId::GestureButton`],
    // the MX Master 4 panel is plain-diverted and its press must deliver that
    // binding — the panel is the gesture button on that model.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let buttons = [(reprog_controls::HAPTIC_PANEL_CID, ButtonId::GestureButton)];

    handle_reprog(&mut acc, panel_press(), &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, release(), &[], &[], &buttons, &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::GestureButton, None))
    );
    assert!(
        rx.try_recv().is_err(),
        "a plain-diverted panel must not also emit a gesture click"
    );
}

#[test]
#[ignore = "RED: MX Master 4 haptic panel capture not implemented yet"]
fn gesture_source_discovery_arms_whatever_the_device_exposes() {
    let raw_xy_flags =
        (reprog_controls::CidFlags::RAW_XY | reprog_controls::CidFlags::DIVERTABLE).bits();
    // MX Master 3S-style entry: the dedicated button, with raw-XY.
    let dedicated = reprog_controls::CtrlIdInfo {
        cid: reprog_controls::GESTURE_BUTTON_CID,
        task_id: 0,
        flags: raw_xy_flags,
    };
    // MX Master 4-style haptic panel entry.
    let panel = reprog_controls::CtrlIdInfo {
        cid: reprog_controls::HAPTIC_PANEL_CID,
        task_id: 0,
        flags: raw_xy_flags,
    };
    // A control without raw-XY can't decode swipes — never a gesture source.
    let no_raw_xy = reprog_controls::CtrlIdInfo {
        cid: reprog_controls::HAPTIC_PANEL_CID,
        task_id: 0,
        flags: reprog_controls::CidFlags::DIVERTABLE.bits(),
    };

    assert_eq!(
        gesture_source_cids(&[dedicated]),
        vec![reprog_controls::GESTURE_BUTTON_CID]
    );
    assert_eq!(
        gesture_source_cids(&[panel]),
        vec![reprog_controls::HAPTIC_PANEL_CID]
    );
    assert_eq!(
        gesture_source_cids(&[dedicated, panel]),
        vec![
            reprog_controls::GESTURE_BUTTON_CID,
            reprog_controls::HAPTIC_PANEL_CID
        ],
        "a device exposing both arms both"
    );
    assert_eq!(
        gesture_source_cids(&[no_raw_xy]),
        Vec::<u16>::new(),
        "raw-XY support is required"
    );
}

#[test]
fn a_plain_diverted_gesture_button_presses_without_gesturing() {
    // A gesture button diverted as a plain button (it does NOT own the gesture
    // role; its single binding needs delivery) must dispatch as a button press
    // only — the swipe accumulator belongs to the raw-XY gesture divert and
    // must not also emit a gesture click on release.
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
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);
    let sources = [reprog_controls::GESTURE_BUTTON_CID];

    handle_reprog(&mut acc, down, &sources, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, &sources, &[dpi], &[], &tx);

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
    let sources = [reprog_controls::GESTURE_BUTTON_CID];

    handle_reprog(&mut acc, down, &sources, &[dpi], &[], &tx);
    handle_reprog(&mut acc, up, &sources, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, &sources, &[dpi], &[], &tx);

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

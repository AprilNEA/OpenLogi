use super::*;

fn press(cid: u16) -> RawControlEvent {
    RawControlEvent::DivertedButtons([cid, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        press(reprog_controls::GESTURE_BUTTON_CID),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        release(),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );

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

    handle_reprog(
        &mut acc,
        press(reprog_controls::GESTURE_BUTTON_CID),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(
        &mut acc,
        release(),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );
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

    handle_reprog(&mut acc, down, None, GestureButtonMode::Native, &[dpi], &tx);
    handle_reprog(&mut acc, down, None, GestureButtonMode::Native, &[dpi], &tx);

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

    handle_reprog(&mut acc, down, None, GestureButtonMode::Native, &[dpi], &tx);
    handle_reprog(&mut acc, up, None, GestureButtonMode::Native, &[dpi], &tx);
    handle_reprog(&mut acc, down, None, GestureButtonMode::Native, &[dpi], &tx);

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
fn m720_switch_apps_control_is_selected_as_the_gesture_button() {
    let controls = [reprog_controls::CtrlIdInfo {
        cid: reprog_controls::M720_GESTURE_BUTTON_CID,
        task_id: 0x00ad,
        flags: 0x0171,
    }];

    assert_eq!(
        find_gesture_cid(&controls),
        Some(reprog_controls::M720_GESTURE_BUTTON_CID)
    );
}

#[test]
fn m720_switch_apps_tap_emits_a_gesture_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = reprog_controls::M720_GESTURE_BUTTON_CID;

    handle_reprog(
        &mut acc,
        press(cid),
        Some(cid),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        release(),
        Some(cid),
        GestureButtonMode::Gestures,
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Click))
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn dedicated_gesture_cid_is_preferred_when_both_are_present() {
    let controls = reprog_controls::GESTURE_BUTTON_CIDS.map(|cid| reprog_controls::CtrlIdInfo {
        cid,
        task_id: 0,
        flags: 0x0171,
    });

    assert_eq!(
        find_gesture_cid(&controls),
        Some(reprog_controls::GESTURE_BUTTON_CID)
    );
}

#[test]
fn switch_apps_without_raw_xy_is_not_treated_as_a_gesture_button() {
    let controls = [reprog_controls::CtrlIdInfo {
        cid: reprog_controls::M720_GESTURE_BUTTON_CID,
        task_id: 0x00ad,
        flags: 0x0011,
    }];

    assert_eq!(find_gesture_cid(&controls), None);
}

#[test]
fn raw_xy_control_without_diversion_is_not_treated_as_a_gesture_button() {
    let controls = [reprog_controls::CtrlIdInfo {
        cid: reprog_controls::M720_GESTURE_BUTTON_CID,
        task_id: 0x00ad,
        flags: 0x0101,
    }];

    assert_eq!(find_gesture_cid(&controls), None);
}

#[test]
fn disabled_gesture_button_emits_nothing_and_ignores_raw_motion() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = reprog_controls::M720_GESTURE_BUTTON_CID;

    handle_reprog(
        &mut acc,
        press(cid),
        Some(cid),
        GestureButtonMode::Disabled,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(cid),
        GestureButtonMode::Disabled,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        release(),
        Some(cid),
        GestureButtonMode::Disabled,
        &[],
        &tx,
    );

    assert!(rx.try_recv().is_err());
    assert!(!acc.swipe.is_holding());
}

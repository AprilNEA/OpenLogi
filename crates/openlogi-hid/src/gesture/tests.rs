use super::*;

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
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
        press(),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        release(),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
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
        press(),
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
        &[],
        &tx,
    );
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
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
        &[],
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

    handle_reprog(&mut acc, down, None, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, None, &[dpi], &[], &tx);

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

    handle_reprog(&mut acc, down, None, &[dpi], &[], &tx);
    handle_reprog(&mut acc, up, None, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, None, &[dpi], &[], &tx);

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
fn haptic_panel_discards_its_contact_jump_then_commits_swipe() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let panel = reprog_controls::HAPTIC_PANEL_CID;
    let down = RawControlEvent::DivertedButtons([panel, 0, 0, 0]);

    handle_reprog(&mut acc, down, Some(panel), &[], &[], &tx);
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 2_000, dy: 0 },
        Some(panel),
        &[],
        &[],
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "the first panel report is an absolute contact position, not a swipe"
    );

    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        Some(panel),
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
fn non_owner_hidpp_source_is_ignored() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let panel = reprog_controls::HAPTIC_PANEL_CID;
    let panel_down = RawControlEvent::DivertedButtons([panel, 0, 0, 0]);

    handle_reprog(
        &mut acc,
        panel_down,
        Some(reprog_controls::GESTURE_BUTTON_CID),
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), None, &[], &[], &tx);

    assert!(rx.try_recv().is_err());
}

#[test]
fn plain_haptic_panel_binding_fires_once_per_press() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let panel = reprog_controls::HAPTIC_PANEL_CID;
    let buttons = [(panel, ButtonId::HapticPanel)];
    let down = RawControlEvent::DivertedButtons([panel, 0, 0, 0]);

    handle_reprog(&mut acc, down, None, &[], &buttons, &tx);
    handle_reprog(&mut acc, down, None, &[], &buttons, &tx);
    handle_reprog(&mut acc, release(), None, &[], &buttons, &tx);
    handle_reprog(&mut acc, down, None, &[], &buttons, &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::HapticPanel))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::HapticPanel))
    );
    assert!(rx.try_recv().is_err());
}

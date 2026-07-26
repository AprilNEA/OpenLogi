use super::*;

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

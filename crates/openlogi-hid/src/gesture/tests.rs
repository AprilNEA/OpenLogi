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

    handle_reprog(&mut acc, press(), &[], &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &[], &tx);

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

    handle_reprog(&mut acc, press(), &[], &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(&mut acc, release(), &[], &[], &tx);
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

    handle_reprog(&mut acc, down, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, &[dpi], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert!(rx.try_recv().is_err(), "a held DPI button presses once");
}

#[test]
fn a_hold_tracked_button_emits_both_edges_exactly_once() {
    // The whole point of diverting these: a press and a release the consumer can
    // time. Repeat frames while the button stays down must not re-emit — the
    // device sends one report per change, but a resend must stay harmless.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = reprog_controls::FORWARD_CID;
    let hold = [(cid, ButtonId::Forward)];
    let down = RawControlEvent::DivertedButtons([cid, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[], &hold, &tx);
    handle_reprog(&mut acc, down, &[], &hold, &tx);
    handle_reprog(&mut acc, up, &[], &hold, &tx);
    handle_reprog(&mut acc, up, &[], &hold, &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Forward))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonReleased(ButtonId::Forward))
    );
    assert!(
        rx.try_recv().is_err(),
        "each edge fires once, however many frames repeat it"
    );
}

#[test]
fn hold_tracking_keeps_the_two_thumb_buttons_independent() {
    // Both diverted at once: one going down must not mask the other's edges,
    // and the frame that holds both is a single press for each.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let back = reprog_controls::BACK_CID;
    let fwd = reprog_controls::FORWARD_CID;
    let hold = [(back, ButtonId::Back), (fwd, ButtonId::Forward)];

    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([back, 0, 0, 0]),
        &[],
        &hold,
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([back, fwd, 0, 0]),
        &[],
        &hold,
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([fwd, 0, 0, 0]),
        &[],
        &hold,
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Back))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Forward))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonReleased(ButtonId::Back)),
        "back leaving the held set is its release, with forward still down"
    );
    assert!(rx.try_recv().is_err(), "forward is still held");
}

#[test]
fn an_undiverted_button_emits_nothing_even_if_the_device_reports_it() {
    // Nothing is opted in, so a frame naming the thumb button must stay silent —
    // otherwise a stray report would fire an action the user never bound to this
    // path.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = reprog_controls::FORWARD_CID;

    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([cid, 0, 0, 0]),
        &[],
        &[],
        &tx,
    );

    assert!(rx.try_recv().is_err(), "hold tracking is opt-in per button");
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

    handle_reprog(&mut acc, down, &[dpi], &[], &tx);
    handle_reprog(&mut acc, up, &[dpi], &[], &tx);
    handle_reprog(&mut acc, down, &[dpi], &[], &tx);

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

use super::*;
use crate::reprog_controls::{AnalyticsKeyEvent, ControlId};

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

/// One analytics batch with a single populated entry (the observed hardware
/// shape: one entry per message, four empty slots).
fn ring(cid: u16, event: u8) -> RawControlEvent {
    let mut entries = [AnalyticsKeyEvent::default(); 5];
    entries[0] = AnalyticsKeyEvent {
        cid: ControlId(cid),
        event,
    };
    RawControlEvent::AnalyticsKeys(entries)
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
fn a_ring_tap_is_one_rising_edge_and_one_press() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x01),
        &[],
        &tx,
    );
    assert!(acc.panel_down, "a press entry arms the edge");
    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x00),
        &[],
        &tx,
    );
    assert!(!acc.panel_down, "a release entry clears the edge");
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::ActionRing))
    );
    assert!(
        rx.try_recv().is_err(),
        "one physical tap must emit exactly one press"
    );
}

#[test]
fn a_ring_tap_re_arms_after_release() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x01),
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x00),
        &[],
        &tx,
    );
    assert!(!acc.panel_down);
    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x01),
        &[],
        &tx,
    );
    assert!(acc.panel_down, "a release re-arms the rising edge");
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::ActionRing))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::ActionRing)),
        "a release re-arms: the second tap presses again"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn diverted_side_buttons_press_on_the_rising_edge() {
    // The MX Master 4's Back/Forward emit no native HID events; the session
    // diverts them, so each hold must dispatch exactly one press.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let back = RawControlEvent::DivertedButtons([reprog_controls::BACK_CID, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, back, &[], &tx);
    handle_reprog(&mut acc, back, &[], &tx); // still held — no repeat
    handle_reprog(&mut acc, up, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([reprog_controls::FORWARD_CID, 0, 0, 0]),
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Back))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::Forward)),
        "a release re-arms; the other button has its own edge"
    );
    assert!(rx.try_recv().is_err(), "a held button presses once");
}

#[test]
fn both_side_buttons_in_one_frame_press_both() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([
            reprog_controls::BACK_CID,
            reprog_controls::FORWARD_CID,
            0,
            0,
        ]),
        &[],
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
    assert!(rx.try_recv().is_err());
}

#[test]
fn click_telemetry_cids_never_open_the_ring() {
    // 0x0050/0x0051 are the LEFT/RIGHT mouse buttons' analytics CIDs, not the
    // pad. If anything arms them (Options+ does, for telemetry), every
    // physical click would arrive here — and must be ignored, or clicking
    // anywhere opens the ring (hardware-confirmed failure, 2026-07-20).
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    for cid in [
        reprog_controls::LEFT_BUTTON_CID,
        reprog_controls::RIGHT_BUTTON_CID,
    ] {
        handle_reprog(&mut acc, ring(cid, 0x01), &[], &tx);
        assert!(!acc.panel_down, "a click press must not arm the ring edge");
        handle_reprog(&mut acc, ring(cid, 0x00), &[], &tx);
    }
    assert!(rx.try_recv().is_err(), "clicks must emit nothing");
}

#[test]
fn analytics_from_foreign_controls_do_not_touch_the_ring_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        ring(reprog_controls::GESTURE_BUTTON_CID, 0x01),
        &[],
        &tx,
    );
    assert!(
        !acc.panel_down,
        "a non-ring CID in an analytics batch is not the pad"
    );
    assert!(rx.try_recv().is_err());
    assert!(
        !acc.swipe.is_holding(),
        "and it must not start a swipe either"
    );
}

#[test]
fn a_batch_with_press_and_release_entries_arms_the_edge() {
    // The wire format carries five entries per message: a pad press plus an
    // unrelated click-CID entry in the same batch must arm the edge off the
    // pad entry alone, with the click entry ignored.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    let mut entries = [AnalyticsKeyEvent::default(); 5];
    entries[0] = AnalyticsKeyEvent {
        cid: ControlId(reprog_controls::ACTION_RING_CID),
        event: 0x01,
    };
    entries[1] = AnalyticsKeyEvent {
        cid: ControlId(reprog_controls::LEFT_BUTTON_CID),
        event: 0x00,
    };
    handle_reprog(&mut acc, RawControlEvent::AnalyticsKeys(entries), &[], &tx);
    assert!(
        acc.panel_down,
        "the press entry wins over the release entry"
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::ActionRing))
    );
    assert!(rx.try_recv().is_err(), "exactly one press for the batch");
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

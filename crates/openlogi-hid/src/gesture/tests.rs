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
fn a_ring_tap_is_one_rising_edge_and_emits_nothing_until_bindable() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x01),
        &[],
        &tx,
    );
    assert!(acc.panel_down, "a press entry arms the edge");
    // A companion CID firing while the pad is held is the same physical press.
    handle_reprog(&mut acc, ring(0x0050, 0x01), &[], &tx);
    assert!(acc.panel_down, "a companion press is not a second edge");
    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x00),
        &[],
        &tx,
    );
    assert!(!acc.panel_down, "a release entry clears the edge");
    assert!(
        rx.try_recv().is_err(),
        "analytics events carry no CapturedInput until the ring is a bindable control"
    );
}

#[test]
fn a_ring_tap_re_arms_after_release() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, ring(0x0050, 0x01), &[], &tx);
    handle_reprog(&mut acc, ring(0x0050, 0x00), &[], &tx);
    assert!(!acc.panel_down);
    // Taps arrive on either ring CID (both observed on hardware) — the edge
    // logic must not care which one carried the previous tap.
    handle_reprog(
        &mut acc,
        ring(reprog_controls::ACTION_RING_CID, 0x01),
        &[],
        &tx,
    );
    assert!(acc.panel_down, "a release re-arms the rising edge");
    assert!(rx.try_recv().is_err());
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
    // The wire format carries five entries per message; if a press and a
    // (stale) release arrive together, the press wins — dropping a tap is
    // worse than clearing the edge a message late.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    let mut entries = [AnalyticsKeyEvent::default(); 5];
    entries[0] = AnalyticsKeyEvent {
        cid: ControlId(reprog_controls::ACTION_RING_CID),
        event: 0x01,
    };
    entries[1] = AnalyticsKeyEvent {
        cid: ControlId(0x0050),
        event: 0x00,
    };
    handle_reprog(&mut acc, RawControlEvent::AnalyticsKeys(entries), &[], &tx);
    assert!(
        acc.panel_down,
        "the press entry wins over the release entry"
    );
    assert!(rx.try_recv().is_err());
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

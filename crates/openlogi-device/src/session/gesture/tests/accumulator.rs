//! The capture accumulator: presses, taps, swipes and raw XY across the gesture sources.

use std::collections::BTreeMap;

use super::*;

const GESTURE: &[u16] = &[reprog_controls::GESTURE_BUTTON_CID];

#[test]
fn alternate_gesture_control_delivers_click_and_swipe() {
    for swipe in [false, true] {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut acc = CaptureAccum::default();
        let sources = &[0x00d0];
        handle_reprog(
            &mut acc,
            RawControlEvent::DivertedButtons([0x00d0, 0, 0, 0]),
            sources,
            &[],
            &[],
            &tx,
        );
        if swipe {
            acc.backdate_hold_for_test();
            stale_report(&mut acc, &tx);
            handle_reprog(
                &mut acc,
                RawControlEvent::RawXy { dx: 120, dy: 5 },
                sources,
                &[],
                &[],
                &tx,
            );
        }
        handle_reprog(&mut acc, release(), sources, &[], &[], &tx);
        assert_eq!(
            next_gesture(&mut rx),
            Ok(CapturedInput::Gesture(
                ButtonId::GestureButton,
                if swipe {
                    GestureDirection::Right
                } else {
                    GestureDirection::Click
                }
            ))
        );
        assert!(
            next_gesture(&mut rx).is_err(),
            "one hold must dispatch only once"
        );
    }
}

const PANEL: &[u16] = &[reprog_controls::HAPTIC_PANEL_CID];

const BOTH: &[u16] = &[
    reprog_controls::GESTURE_BUTTON_CID,
    reprog_controls::HAPTIC_PANEL_CID,
];

/// Feed the stale first report every fresh hold opens with: travel from
/// before the press, far past every threshold and pointing away from any
/// stroke these tests make.
fn stale_report(acc: &mut CaptureAccum, tx: &mpsc::UnboundedSender<CapturedInput>) {
    let stale = RawControlEvent::RawXy {
        dx: -3000,
        dy: 2400,
    };
    handle_reprog(acc, stale, &[], &[], &[], tx);
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

/// Read the next completed gesture while leaving lifecycle assertions to the
/// dedicated edge tests below.
fn next_gesture(
    rx: &mut mpsc::UnboundedReceiver<CapturedInput>,
) -> Result<CapturedInput, mpsc::error::TryRecvError> {
    loop {
        let input = rx.try_recv()?;
        if matches!(input, CapturedInput::Gesture(..)) {
            return Ok(input);
        }
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "the released holder still clicks"
    );

    acc.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        )),
        "the taken-over hold dispatches through the panel's own map"
    );

    handle_reprog(&mut acc, release(), BOTH, &[], &[], &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
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
    acc.backdate_hold_for_test();
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
        next_gesture(&mut rx).is_err(),
        "ambiguous overlap motion must not commit a swipe"
    );

    // The panel lifts; the surviving hold accumulates again.
    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    acc.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        BOTH,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        next_gesture(&mut rx),
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "the swapped-out holder still clicks"
    );

    acc.backdate_hold_for_test();
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
        next_gesture(&mut rx).is_err(),
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
        next_gesture(&mut rx),
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
    // Past the stale report, so the travel below reaches the accumulator and
    // only the hold-time gate stands between it and a swipe.
    stale_report(&mut acc, &tx);
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        ))
    );
    assert!(
        next_gesture(&mut rx).is_err(),
        "a quick tap emits exactly one click"
    );
}

#[test]
fn a_held_gesture_commits_a_swipe_and_does_not_also_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    stale_report(&mut acc, &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        GESTURE,
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        ))
    );

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
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
    acc.backdate_hold_for_test();
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Up
        ))
    );

    handle_reprog(&mut acc, release(), PANEL, &[], &[], &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Click
        ))
    );
    assert!(
        next_gesture(&mut rx).is_err(),
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
    acc.backdate_hold_for_test();
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
        next_gesture(&mut rx).is_err(),
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
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        ))
    );
}

#[test]
fn the_dedicated_buttons_first_sample_is_discarded_as_stale() {
    // Recorded on an MX Master 3S over Bluetooth: the first raw-XY report of
    // each hold carried the travel since the previous hold's last report —
    // here the hand's return stroke, leftward and far past every threshold —
    // before the rightward swipe the hold was actually making.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let xy = |dx, dy| RawControlEvent::RawXy { dx, dy };

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    acc.backdate_hold_for_test();
    handle_reprog(&mut acc, xy(-2717, -105), GESTURE, &[], &[], &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
        "travel from before the press must not commit a direction"
    );
    for dx in [115, 49, 76, 66] {
        handle_reprog(&mut acc, xy(dx, 2), GESTURE, &[], &[], &tx);
    }
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Right
        )),
        "the stroke made during the hold decides"
    );

    // Every press opens with its own stale report, not just the first.
    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    acc.backdate_hold_for_test();
    handle_reprog(&mut acc, xy(633, -1234), GESTURE, &[], &[], &tx);
    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "a hold with only its stale report is a click"
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
    acc.backdate_hold_for_test();
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
        next_gesture(&mut rx).is_err(),
        "a non-owner source must neither gesture nor click"
    );
}

#[test]
fn gesture_sources_emit_independent_edges_without_snapshot_duplicates() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, both_press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, panel_press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, release(), BOTH, &[], &[], &tx);

    let edges: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|input| !matches!(input, CapturedInput::Gesture(..)))
        .collect();
    assert_eq!(
        edges,
        vec![
            CapturedInput::ButtonDown(ButtonId::GestureButton),
            CapturedInput::ButtonDown(ButtonId::HapticPanel),
            CapturedInput::ButtonUp(ButtonId::GestureButton),
            CapturedInput::ButtonUp(ButtonId::HapticPanel),
        ]
    );
}

#[test]
fn a_plain_diverted_gesture_button_presses_without_gesturing() {
    // A gesture button diverted as a plain button (not in gesture mode; its
    // single binding needs delivery) must dispatch as a button press only —
    // the swipe accumulator belongs to the raw-XY gesture diverts and must
    // not also emit a gesture click on release.
    for cid in [0x00c3, 0x00d0] {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut acc = CaptureAccum::default();
        let buttons = [(cid, ButtonId::GestureButton)];
        handle_reprog(
            &mut acc,
            RawControlEvent::DivertedButtons([cid, 0, 0, 0]),
            &[],
            &[],
            &buttons,
            &tx,
        );
        handle_reprog(
            &mut acc,
            RawControlEvent::RawXy { dx: 120, dy: 5 },
            &[],
            &[],
            &buttons,
            &tx,
        );
        handle_reprog(&mut acc, release(), &[], &[], &buttons, &tx);

        assert_eq!(
            rx.try_recv(),
            Ok(CapturedInput::ButtonDown(ButtonId::GestureButton))
        );
        assert_eq!(
            rx.try_recv(),
            Ok(CapturedInput::ButtonUp(ButtonId::GestureButton))
        );
        assert!(
            rx.try_recv().is_err(),
            "a plain-diverted gesture button must not also emit a gesture click"
        );
    }
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
        Ok(CapturedInput::ButtonDown(ButtonId::HapticPanel))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::HapticPanel))
    );
    assert!(
        rx.try_recv().is_err(),
        "a plain-diverted panel must not also emit a gesture click"
    );
}

#[test]
fn plain_button_snapshots_emit_each_edge_once_and_keep_buttons_independent() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let buttons = [(0x0053, ButtonId::Back), (0x0056, ButtonId::Forward)];
    let back = RawControlEvent::DivertedButtons([0x0053, 0, 0, 0]);
    let both = RawControlEvent::DivertedButtons([0x0053, 0x0056, 0, 0]);
    let forward = RawControlEvent::DivertedButtons([0x0056, 0, 0, 0]);

    handle_reprog(&mut acc, back, &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, back, &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, both, &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, forward, &[], &[], &buttons, &tx);
    handle_reprog(&mut acc, release(), &[], &[], &buttons, &tx);

    assert_eq!(rx.try_recv(), Ok(CapturedInput::ButtonDown(ButtonId::Back)));
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::Forward))
    );
    assert_eq!(rx.try_recv(), Ok(CapturedInput::ButtonUp(ButtonId::Back)));
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::Forward))
    );
    assert!(rx.try_recv().is_err(), "unchanged snapshots emit no edges");
}

#[test]
fn a_side_gesture_button_uses_its_hidpp_raw_xy() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = 0x0056;
    let buttons = [(cid, ButtonId::Forward)];
    let down = RawControlEvent::DivertedButtons([cid, 0, 0, 0]);

    acc.on_event(down, &[], &[], &buttons, &[], &tx);
    stale_report(&mut acc, &tx);
    acc.backdate_hold_for_test();
    acc.on_event(
        RawControlEvent::RawXy { dx: -120, dy: 5 },
        &[],
        &[],
        &buttons,
        &[],
        &tx,
    );
    acc.on_event(release(), &[], &[], &buttons, &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::Forward))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::Forward,
            GestureDirection::Left
        ))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::Forward))
    );
    assert!(
        rx.try_recv().is_err(),
        "a committed side-button swipe must not also click on release"
    );
}

#[test]
fn a_side_gesture_button_tap_is_a_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = 0x0056;
    let buttons = [(cid, ButtonId::Forward)];
    let down = RawControlEvent::DivertedButtons([cid, 0, 0, 0]);

    acc.on_event(down, &[], &[], &buttons, &[], &tx);
    acc.on_event(release(), &[], &[], &buttons, &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::Forward))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::Forward,
            GestureDirection::Click
        ))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::Forward))
    );
    assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

#[test]
fn a_dpi_gesture_button_uses_the_shared_raw_xy_path() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let cid = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let buttons = [(cid, ButtonId::DpiToggle)];
    let down = RawControlEvent::DivertedButtons([cid, 0, 0, 0]);

    acc.on_event(down, &[], &[], &buttons, &[], &tx);
    stale_report(&mut acc, &tx);
    acc.backdate_hold_for_test();
    acc.on_event(
        RawControlEvent::RawXy { dx: 5, dy: -120 },
        &[],
        &[],
        &buttons,
        &[],
        &tx,
    );
    acc.on_event(release(), &[], &[], &buttons, &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::DpiToggle))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::DpiToggle,
            GestureDirection::Up
        ))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::DpiToggle))
    );
    assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
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
        Ok(CapturedInput::ButtonDown(ButtonId::DpiToggle))
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
        Ok(CapturedInput::ButtonDown(ButtonId::DpiToggle))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::DpiToggle)),
        "the falling edge must be preserved"
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::DpiToggle)),
        "a release re-arms the rising edge"
    );
    assert!(
        rx.try_recv().is_err(),
        "press → release → press emits exactly three lifecycle edges"
    );
}

const BACK: u16 = 0x0053;
const FORWARD: u16 = 0x0056;
const WHEEL_BUTTONS: &[(u16, ButtonId)] = &[(BACK, ButtonId::Back), (FORWARD, ButtonId::Forward)];

fn hold(acc: &mut CaptureAccum, cids: [u16; 4], tx: &mpsc::UnboundedSender<CapturedInput>) {
    handle_reprog(
        acc,
        RawControlEvent::DivertedButtons(cids),
        &[],
        &[],
        WHEEL_BUTTONS,
        tx,
    );
}

fn raw_wheel(
    resolution: reprog_controls::RawWheelResolution,
    delta: i16,
) -> reprog_controls::RawWheelEvent {
    reprog_controls::RawWheelEvent { resolution, delta }
}

fn held_wheel(rx: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|input| matches!(input, CapturedInput::HeldWheel { .. }))
        .collect()
}

#[test]
fn raw_wheel_under_a_held_wheel_hold_button_scrolls_for_it() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold(&mut acc, [BACK, 0, 0, 0], &tx);
    let eight = NonZeroU8::new(8);
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, -2),
        WHEEL_BUTTONS,
        eight,
        &tx,
    );
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::High, 5),
        WHEEL_BUTTONS,
        eight,
        &tx,
    );
    assert_eq!(
        held_wheel(&mut rx),
        [
            CapturedInput::HeldWheel {
                button: ButtonId::Back,
                delta: -2,
                per_notch: NonZeroU8::MIN
            },
            CapturedInput::HeldWheel {
                button: ButtonId::Back,
                delta: 5,
                per_notch: NonZeroU8::new(8).expect("non-zero")
            },
        ]
    );
}

#[test]
fn raw_wheel_belongs_to_the_earliest_pressed_wheel_button() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold(&mut acc, [FORWARD, 0, 0, 0], &tx);
    hold(&mut acc, [BACK, FORWARD, 0, 0], &tx);
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, 1),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    hold(&mut acc, [BACK, 0, 0, 0], &tx);
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, 1),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    let buttons: Vec<_> = held_wheel(&mut rx)
        .into_iter()
        .map(|input| match input {
            CapturedInput::HeldWheel { button, .. } => button,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(buttons, [ButtonId::Forward, ButtonId::Back]);
}

#[test]
fn raw_wheel_that_cannot_be_attributed_or_scaled_is_dropped() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    // Nothing held: a release crossed the report in flight.
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, 3),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    // Held, but a plain-diverted button is not a wheel-hold button.
    handle_reprog(
        &mut acc,
        RawControlEvent::DivertedButtons([0x0052, 0, 0, 0]),
        &[],
        &[],
        &[(0x0052, ButtonId::MiddleClick)],
        &tx,
    );
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, 3),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    hold(&mut acc, [BACK, 0, 0, 0], &tx);
    // No movement, and high resolution with no known multiplier.
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::Low, 0),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    acc.on_raw_wheel(
        raw_wheel(reprog_controls::RawWheelResolution::High, 8),
        WHEEL_BUTTONS,
        None,
        &tx,
    );
    assert_eq!(held_wheel(&mut rx), []);
}

fn per_notch() -> NonZeroU8 {
    NonZeroU8::new(8).expect("non-zero")
}

fn hold_gesture(acc: &mut CaptureAccum, tx: &mpsc::UnboundedSender<CapturedInput>) {
    handle_reprog(acc, press(), GESTURE, &[], &[], tx);
}

fn wheel_at(
    acc: &mut CaptureAccum,
    delta: i16,
    enabled: bool,
    tx: &mpsc::UnboundedSender<CapturedInput>,
) -> bool {
    acc.on_main_wheel(delta, per_notch(), enabled, &BTreeMap::new(), tx)
}

fn wheel_each(
    acc: &mut CaptureAccum,
    delta: i16,
    tx: &mpsc::UnboundedSender<CapturedInput>,
) -> bool {
    let mut each = BTreeMap::new();
    each.insert(
        ButtonId::GestureButton,
        WheelNotchRepeat {
            up: true,
            down: true,
        },
    );
    acc.on_main_wheel(delta, per_notch(), true, &each, tx)
}

fn assert_wheel(rx: &mut mpsc::UnboundedReceiver<CapturedInput>, direction: GestureDirection) {
    assert_eq!(
        next_gesture(rx),
        Ok(CapturedInput::Gesture(ButtonId::GestureButton, direction))
    );
}

fn assert_no_gesture(rx: &mut mpsc::UnboundedReceiver<CapturedInput>) {
    match next_gesture(rx) {
        Err(mpsc::error::TryRecvError::Empty) => {}
        other => panic!("expected no gesture, got {other:?}"),
    }
}

fn assert_wheels(
    rx: &mut mpsc::UnboundedReceiver<CapturedInput>,
    direction: GestureDirection,
    count: usize,
) {
    for _ in 0..count {
        assert_wheel(rx, direction);
    }
    assert_no_gesture(rx);
}

/// Eight high-resolution units are one notch. Seven do not fire; the eighth
/// fires one `WheelUp`. A further notch is not saved for later.
#[test]
fn sub_notch_samples_sum_to_one_wheel_up_and_a_blocked_notch_is_not_banked() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    for _ in 0..7 {
        assert!(wheel_at(&mut acc, 1, true, &tx));
    }
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, 1, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, 1, true, &tx));
    assert_no_gesture(&mut rx);
}

/// One free-spin report fires one gesture when the direction does not repeat.
/// The notches past the first are discarded.
#[test]
fn one_report_of_many_notches_fires_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 120, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, 120, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, 1, true, &tx));
    assert_no_gesture(&mut rx);
}

/// A direction that does not repeat fires once for the hold, not once per notch.
#[test]
fn a_second_notch_in_the_same_direction_does_not_fire() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_no_gesture(&mut rx);
}

/// After the one allowed notch, a later sample keeps only its fraction. That
/// fraction must not complete a second step in the same direction.
#[test]
fn a_limited_direction_keeps_the_fraction_but_does_not_fire_again() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    // 12 units is 1.5 notches. The whole one is consumed; half a notch remains.
    assert!(wheel_at(&mut acc, 12, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, 4, true, &tx));
    assert_no_gesture(&mut rx);
}

/// The leftover fraction can still be cancelled, and the other direction then
/// gets its own single step.
#[test]
fn a_leftover_fraction_can_still_be_cancelled() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);
    assert!(wheel_at(&mut acc, 12, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, -4, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, -8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelDown);
}

/// 120 units at 8 per notch is fifteen steps, and the other way counts too.
#[test]
fn each_notch_mode_fires_every_whole_notch_in_the_report() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_each(&mut acc, 120, &tx));
    assert_wheels(&mut rx, GestureDirection::WheelUp, 15);

    assert!(wheel_each(&mut acc, -16, &tx));
    assert_wheels(&mut rx, GestureDirection::WheelDown, 2);

    assert!(wheel_each(&mut acc, 8, &tx));
    assert_wheels(&mut rx, GestureDirection::WheelUp, 1);

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert_no_gesture(&mut rx);
}

/// Whole notches past the cap are dropped. The next report starts clean.
#[test]
fn a_report_past_the_cap_drops_the_extra_notches() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);
    let mut each = BTreeMap::new();
    each.insert(
        ButtonId::GestureButton,
        WheelNotchRepeat {
            up: true,
            down: true,
        },
    );
    let one = NonZeroU8::new(1).expect("non-zero");

    assert!(acc.on_main_wheel(100, one, true, &each, &tx));
    assert_wheels(&mut rx, GestureDirection::WheelUp, 48);

    assert!(acc.on_main_wheel(1, one, true, &each, &tx));
    assert_wheels(&mut rx, GestureDirection::WheelUp, 1);
}

#[test]
fn reversing_through_zero_cancels_a_partial_notch() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 4, true, &tx));
    assert!(wheel_at(&mut acc, -4, true, &tx));
    assert_no_gesture(&mut rx);

    assert!(wheel_at(&mut acc, -8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelDown);
}

#[test]
fn sub_threshold_wheel_motion_still_clicks_on_release() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 1, true, &tx));
    assert_no_gesture(&mut rx);

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert_wheel(&mut rx, GestureDirection::Click);

    assert!(!wheel_at(&mut acc, 8, true, &tx));
    assert_no_gesture(&mut rx);
}

#[test]
fn a_committed_wheel_direction_suppresses_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert_no_gesture(&mut rx);
}

#[test]
fn a_disabled_wheel_gesture_leaves_the_sample_and_the_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(!wheel_at(&mut acc, 8, false, &tx));
    assert_no_gesture(&mut rx);

    handle_reprog(&mut acc, release(), GESTURE, &[], &[], &tx);
    assert_wheel(&mut rx, GestureDirection::Click);
}

#[test]
fn wheel_motion_outside_a_hold_is_not_consumed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    assert!(!wheel_at(&mut acc, 8, true, &tx));
    assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

#[test]
fn wheel_sign_follows_the_report_and_each_direction_fires_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    hold_gesture(&mut acc, &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    assert!(wheel_at(&mut acc, -8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelDown);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_no_gesture(&mut rx);
}

#[test]
fn wheel_motion_during_overlap_stays_with_the_first_holder() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    handle_reprog(&mut acc, press(), BOTH, &[], &[], &tx);
    handle_reprog(&mut acc, both_press(), BOTH, &[], &[], &tx);

    assert!(wheel_at(&mut acc, 8, true, &tx));
    assert_wheel(&mut rx, GestureDirection::WheelUp);

    handle_reprog(&mut acc, release(), BOTH, &[], &[], &tx);
    assert_no_gesture(&mut rx);
}

//! The capture accumulator: presses, taps, swipes and raw XY across the gesture sources.

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

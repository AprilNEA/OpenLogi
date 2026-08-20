use super::*;

const GESTURE: &[u16] = &[reprog_controls::GESTURE_BUTTON_CID];
const PANEL: &[u16] = &[reprog_controls::HAPTIC_PANEL_CID];
const BOTH: &[u16] = &[
    reprog_controls::GESTURE_BUTTON_CID,
    reprog_controls::HAPTIC_PANEL_CID,
];

fn reporting(
    diverted: bool,
    remap: Option<reprog_controls::ControlId>,
) -> reprog_controls::CidReporting {
    reprog_controls::CidReporting {
        cid: reprog_controls::ControlId(reprog_controls::GESTURE_BUTTON_CID),
        diverted,
        persistently_diverted: true,
        force_raw_xy: true,
        raw_xy: true,
        remap,
        analytics_key_events: true,
        raw_wheel: true,
    }
}

#[test]
fn requested_mode_is_native_when_a_control_is_absent() {
    assert_eq!(
        requested_reprog_mode(&CaptureSpec::default(), reprog_controls::GESTURE_BUTTON_CID),
        ReprogMode::Native
    );
}

#[test]
fn requested_mode_is_plain_for_a_diverted_button() {
    let spec = CaptureSpec {
        divert_buttons: vec![(reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton)],
        ..CaptureSpec::default()
    };

    assert_eq!(
        requested_reprog_mode(&spec, reprog_controls::GESTURE_BUTTON_CID),
        ReprogMode::Plain(ButtonId::GestureButton)
    );
}

#[test]
fn raw_xy_takes_precedence_over_a_duplicate_plain_divert() {
    let spec = CaptureSpec {
        divert_gesture_sources: vec![reprog_controls::GESTURE_BUTTON_CID],
        divert_buttons: vec![(reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton)],
        ..CaptureSpec::default()
    };

    assert_eq!(
        requested_reprog_mode(&spec, reprog_controls::GESTURE_BUTTON_CID),
        ReprogMode::RawXy
    );
}

#[tokio::test]
async fn removing_a_gesture_source_resets_its_in_progress_hold() {
    let mut armed = ArmedControls {
        gesture_cids: GESTURE.to_vec(),
        ..ArmedControls::default()
    };
    let runtime = Mutex::new(CaptureRuntimeState {
        gesture_cids: GESTURE.to_vec(),
        accum: CaptureAccum {
            gesture_source: Some((reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton)),
            overlap: true,
            gestures_down: GESTURE.to_vec(),
            skip_first_raw_xy: true,
            ..CaptureAccum::default()
        },
        ..CaptureRuntimeState::default()
    });

    let Ok(()) = apply_spec_update(&mut armed, &CaptureSpec::default(), &runtime, 1).await else {
        panic!("a hardware-free update should succeed");
    };

    let runtime = runtime.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(runtime.gesture_cids.is_empty());
    assert!(runtime.accum.gesture_source.is_none());
    assert!(!runtime.accum.overlap);
    assert!(runtime.accum.gestures_down.is_empty());
    assert!(!runtime.accum.skip_first_raw_xy);
}

/// A control that was *already* diverted when the session armed it — an agent
/// killed mid-session, or another Logitech app — must not be handed that state
/// back. Replaying it leaves the button diverted with no listener: no OS event
/// and no HID++ consumer, dead until the device sleeps.
#[test]
fn restore_clears_a_diversion_it_found_already_set() {
    let change = undivert_change(reporting(true, None));

    assert_eq!(change.diverted, Some(false));
    assert_eq!(change.raw_xy, Some(false));
}

/// Arming only ever writes `diverted` / `raw_xy` and re-asserts `remap`, so
/// restoring must leave every other bit alone rather than writing back a
/// snapshot that may itself be this session's leftovers.
#[test]
fn restore_returns_the_remap_target_and_touches_nothing_else() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = undivert_change(reporting(false, Some(remap)));

    assert_eq!(change.remap, Some(remap));
    assert_eq!(change.persistently_diverted, None);
    assert_eq!(change.force_raw_xy, None);
    assert_eq!(change.analytics_key_events, None);
    assert_eq!(change.raw_wheel, None);
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
    assert!(
        rx.try_recv().is_err(),
        "press → release → press emits exactly two presses"
    );
}

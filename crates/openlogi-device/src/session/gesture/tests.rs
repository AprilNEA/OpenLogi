use openlogi_core::binding::hold_drag_threshold_counts;
use openlogi_core::hid::Dpi;

use super::*;
use crate::backend::NodeId;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

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

#[tokio::test]
async fn pending_restore_waits_for_a_replacement_then_undiverts_through_it() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let (retired_raw, retired_handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    let retired = SharedChannel::new(retired_channel.clone(), route.clone());
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    registry.replace_node(node.clone(), [route.clone()], retired_channel);
    let pending = PendingCaptureRestore::new(
        &retired,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
        None,
    )
    .expect("one diverted control should require restoration");

    let pending = match pending.retry(&registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("the retired transport must never restore underneath a replacement")
        }
    };
    assert!(retired_handle.written_reports().is_empty());

    let (replacement_raw, replacement_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let replacement = scripted_channel(replacement_raw).await;
    registry.replace_node(node, [route.clone()], replacement);

    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    let reports = replacement_handle.written_reports();
    assert_eq!(reports.len(), 1);
    let restore = &reports[0];
    assert_eq!(restore[2], 0x22, "restore must address ReprogControlsV4");
    assert_eq!(restore[3] >> 4, 3, "restore must call setCidReporting");
    assert_eq!(
        &restore[4..7],
        &[0x00, 0xc3, 0x22],
        "restore must clear diversion and raw-XY using their valid bits"
    );
}

#[tokio::test]
async fn restore_retries_when_inventory_changes_during_an_awaited_write() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    let retired = SharedChannel::new(retired_channel, route.clone());
    let pending = PendingCaptureRestore::new(
        &retired,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
        None,
    )
    .expect("one diverted control should require restoration");

    let (winner_raw, winner_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let winner = scripted_channel(winner_raw).await;
    let replacement_registry = registry.clone();
    let replacement_node = node.clone();
    let replacement_route = route.clone();
    let replacement_winner = winner.clone();
    let (superseded_raw, superseded_handle) =
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            replacement_registry.replace_node(
                replacement_node.clone(),
                [replacement_route.clone()],
                replacement_winner.clone(),
            );
            Some(request.to_vec())
        });
    let superseded = scripted_channel(superseded_raw).await;
    registry.replace_node(node, [route], superseded);

    let pending = match pending.retry(&registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("a write to a publication replaced during await is not final")
        }
    };
    assert_eq!(superseded_handle.written_reports().len(), 1);
    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(winner_handle.written_reports().len(), 1);
}

#[tokio::test]
async fn failed_setup_rollback_returns_its_restore_capability() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let (raw, _) =
        ScriptedRawHidChannel::with_failing_writes(|request| Some(request.to_vec()), |_| true);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(channel, route.clone());
    let pending = PendingCaptureRestore::new(
        &shared,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
        None,
    );

    let failure = rollback_capture_start(
        GestureError::Hidpp("diversion failed".into()),
        pending,
        &shared,
        None,
    )
    .await;
    let (_, pending) = failure.into_parts();

    assert!(
        pending.is_some(),
        "a failed compensating write must not discard firmware ownership"
    );
}

#[tokio::test]
async fn capture_listener_outlives_native_reporting_restore() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (restored_tx, restored_rx) = oneshot::channel();
    let task = tokio::spawn(drop_listener_after(
        DropProbe(Arc::clone(&dropped)),
        async move {
            let _ = restored_rx.await;
        },
    ));

    tokio::task::yield_now().await;
    assert!(
        !dropped.load(std::sync::atomic::Ordering::Relaxed),
        "the listener must remain installed while native reporting is still diverted"
    );

    restored_tx.send(()).expect("restore signal should be open");
    task.await.expect("listener-retirement task should finish");
    assert!(
        dropped.load(std::sync::atomic::Ordering::Relaxed),
        "the listener may be removed after native reporting is restored"
    );
}

#[tokio::test(start_paused = true)]
async fn channel_change_takes_teardown_precedence_over_ready_shutdown() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    registry.replace_node(node.clone(), [route.clone()], retired_channel);
    let retired = registry
        .lookup(&route)
        .expect("the capture publication should be current");

    let (replacement_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let replacement = scripted_channel(replacement_raw).await;
    registry.replace_node(node, [route], replacement);

    // These are the two monitor branches that can become ready together. The
    // explicit shutdown branch re-checks the publication, so both preserve the
    // typed replacement teardown rather than restoring through `retired`.
    assert!(matches!(
        stop_for_current_publication(Some(&registry), &retired),
        CaptureStop::ChannelChanged
    ));
    assert!(matches!(
        wait_for_channel_change(Some(&registry), &retired).await,
        CaptureStop::ChannelChanged
    ));
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

#[test]
fn wake_rearm_restores_diversion_mode_and_remap_target() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = divert_change(reporting(false, Some(remap)), true);

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(true));
    assert_eq!(change.remap, Some(remap));
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
fn the_dedicated_buttons_first_sample_is_not_discarded() {
    // The discard is a panel quirk: the dedicated button's raw-XY stream is
    // relative from the first sample, which must keep committing as-is.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
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
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let buttons = [(reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton)];

    handle_reprog(&mut acc, press(), &[], &[], &buttons, &tx);
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

    handle_reprog_with_gesture_buttons(&mut acc, down, &[], &[], &buttons, &[], &tx);
    acc.backdate_hold_for_test();
    handle_reprog_with_gesture_buttons(
        &mut acc,
        RawControlEvent::RawXy { dx: -120, dy: 5 },
        &[],
        &[],
        &buttons,
        &[],
        &tx,
    );
    handle_reprog_with_gesture_buttons(&mut acc, release(), &[], &[], &buttons, &[], &tx);

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

    handle_reprog_with_gesture_buttons(&mut acc, down, &[], &[], &buttons, &[], &tx);
    handle_reprog_with_gesture_buttons(&mut acc, release(), &[], &[], &buttons, &[], &tx);

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

/// The resolutions an MX Master 4 reports: 20 ratchets natively, 120
/// increments diverted, so one increment is a sixth of a native scroll unit.
const TRACED_RES: thumbwheel::WheelResolution = thumbwheel::WheelResolution {
    native_res: 20,
    diverted_res: 120,
};

fn thumb_event(
    rotation: i16,
    rotation_status: thumbwheel::RotationStatus,
    single_tap: bool,
) -> thumbwheel::ThumbwheelEvent {
    thumbwheel::ThumbwheelEvent {
        rotation,
        rotation_status,
        single_tap,
        touch: true,
        proxy: true,
    }
}

/// The wheel's touch sensor flags a tap on the same contact that rolled it, so
/// a rolling report's tap bit is an artifact — forwarding it fired the tap's
/// bound action in the middle of a scroll.
#[test]
fn a_rolling_report_is_a_roll_even_when_it_flags_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(-3, thumbwheel::RotationStatus::Active, true),
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: -3,
            resolution: TRACED_RES
        })
    );
}

/// The roll's closing report: the finger lifts, so it carries no rotation of
/// its own while the sensor flags the contact that just rolled the wheel. Only
/// `rotation_status` separates it from a deliberate tap.
#[test]
fn the_release_that_ends_a_roll_is_not_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Stop, true),
            TRACED_RES
        ),
        None
    );
}

#[test]
fn a_tap_on_a_settled_wheel_is_a_tap() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Inactive, true),
            TRACED_RES
        ),
        Some(CapturedInput::ButtonPulse(ButtonId::Thumbwheel))
    );
}

/// A wheel whose firmware leaves byte 4 at zero still has its own rotation to
/// go on, so the roll is recognised without the status field.
#[test]
fn rotation_alone_still_marks_a_roll() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(4, thumbwheel::RotationStatus::Inactive, true),
            TRACED_RES
        ),
        Some(CapturedInput::Scroll {
            increments: 4,
            resolution: TRACED_RES
        })
    );
}

/// Touch and proximity alone carry no input: the wheel reports them whenever a
/// thumb rests near it.
#[test]
fn contact_without_rotation_or_a_tap_carries_no_input() {
    assert_eq!(
        thumbwheel_input(
            thumb_event(0, thumbwheel::RotationStatus::Inactive, false),
            TRACED_RES
        ),
        None
    );
}

const HOLD_BACK: &[(u16, ButtonId)] = &[(0x0053, ButtonId::Back)];
const HOLD_DPI: Dpi = Dpi::new(1000);

fn hold_down() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0x0053, 0, 0, 0])
}

fn handle_hold(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    handle_reprog_sets(
        acc,
        event,
        &ReprogSets {
            gesture_cids: &[],
            dpi_cids: &[],
            gesture_button_cids: &[],
            button_cids: &[],
            hold_button_cids: HOLD_BACK,
            sensor_dpi: Some(HOLD_DPI),
        },
        sink,
    );
}

fn drain(rx: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[test]
fn hold_mode_streams_raw_xy_and_never_fires_a_gesture_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(
        &mut acc,
        RawControlEvent::RawXy {
            dx: 9_000,
            dy: 9_000,
        },
        &tx,
    );
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 40, dy: -15 }, &tx);
    handle_hold(&mut acc, release(), &tx);

    assert_eq!(
        drain(&mut rx),
        vec![
            CapturedInput::HoldBegin(ButtonId::Back),
            CapturedInput::HoldMotion {
                button: ButtonId::Back,
                dx: 40,
                dy: -15
            },
            CapturedInput::HoldEnd {
                button: ButtonId::Back,
                release: HoldRelease::Released { traveled: false }
            },
        ]
    );
}

#[test]
fn hold_mode_no_drag_release_is_not_a_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 2, dy: 1 }, &tx);
    handle_hold(&mut acc, release(), &tx);

    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .all(|input| !matches!(input, CapturedInput::Gesture(..))),
        "hold-mode travel must never fire Gesture(..., Click): {events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Released { traveled: false }
        })
    );
}

#[test]
fn hold_mode_travel_past_the_physical_deadzone_is_traveled() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let step = i16::try_from(hold_drag_threshold_counts(HOLD_DPI) + 1).expect("threshold fits i16");

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(
        &mut acc,
        RawControlEvent::RawXy {
            dx: 9_000,
            dy: 9_000,
        },
        &tx,
    );
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: step, dy: 0 }, &tx);
    handle_hold(&mut acc, release(), &tx);

    assert_eq!(
        drain(&mut rx).last(),
        Some(&CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Released { traveled: true }
        })
    );
}

#[test]
fn hold_mode_overlap_drops_unattributed_raw_xy() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let hold_both = &[(0x0053, ButtonId::Back), (0x0056, ButtonId::Forward)];
    let both = RawControlEvent::DivertedButtons([0x0053, 0x0056, 0, 0]);
    let sets = ReprogSets {
        gesture_cids: &[],
        dpi_cids: &[],
        gesture_button_cids: &[],
        button_cids: &[],
        hold_button_cids: hold_both,
        sensor_dpi: Some(HOLD_DPI),
    };

    handle_reprog_sets(&mut acc, hold_down(), &sets, &tx);
    handle_reprog_sets(&mut acc, both, &sets, &tx);
    handle_reprog_sets(
        &mut acc,
        RawControlEvent::RawXy { dx: 80, dy: 80 },
        &sets,
        &tx,
    );

    assert!(
        drain(&mut rx)
            .iter()
            .all(|input| !matches!(input, CapturedInput::HoldMotion { .. })),
        "overlap motion must be dropped, not attributed"
    );
}

#[test]
fn reconnect_emits_hold_end_before_wiping_the_stream() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 8, dy: 0 }, &tx);
    let _ = drain(&mut rx);

    let end = acc
        .take_terminal_stream_end()
        .expect("an open hold-mode stream must emit its end before wipe");
    assert_eq!(
        end,
        CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Interrupted
        },
        "a wipe closes the stream under a control that is still down"
    );

    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 80, dy: 0 }, &tx);
    assert!(
        drain(&mut rx).is_empty(),
        "late motion after the terminal end must not re-open the stream"
    );

    handle_hold(&mut acc, hold_down(), &tx);
    assert!(
        drain(&mut rx).is_empty(),
        "a control still down after the wipe is not a new press"
    );

    handle_hold(&mut acc, release(), &tx);
    handle_hold(&mut acc, hold_down(), &tx);
    assert_eq!(
        drain(&mut rx).first(),
        Some(&CapturedInput::HoldBegin(ButtonId::Back)),
        "a later rising edge after a real release must still begin a hold"
    );
}

#[test]
fn already_down_hold_control_is_not_a_press() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    acc.seed_hold_already_down_for_test(&[0x0053]);

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 80, dy: 0 }, &tx);
    handle_hold(&mut acc, release(), &tx);

    assert!(
        drain(&mut rx).is_empty(),
        "a control already down when the session starts is not a press"
    );
}

#[test]
fn a_stale_hold_expires_without_waiting_for_release() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    let _ = drain(&mut rx);
    acc.backdate_hold_past_stale_for_test();
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 1, dy: 0 }, &tx);

    assert_eq!(
        drain(&mut rx),
        vec![CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Interrupted
        }],
        "a dropped release must expire the hold rather than stream forever"
    );
}

#[test]
fn a_hold_streaming_motion_never_expires_however_long_it_runs() {
    // The bound was measured from the press, so a pan held past HOLD_STALE
    // was force-ended mid-gesture: reproduced on hardware as a pan that quit
    // 10.14 s after button-down while the user was still dragging.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 9_000, dy: 0 }, &tx);
    // Held far longer than the bound, still streaming.
    acc.backdate_press_past_stale_for_test();
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 500, dy: 0 }, &tx);
    acc.backdate_press_past_stale_for_test();
    handle_hold(&mut acc, RawControlEvent::RawXy { dx: 500, dy: 0 }, &tx);
    handle_hold(&mut acc, release(), &tx);

    let events = drain(&mut rx);
    let ends = events
        .iter()
        .filter(|input| matches!(input, CapturedInput::HoldEnd { .. }))
        .count();
    assert_eq!(ends, 1, "one release, one end: {events:?}");
    assert_eq!(
        events.last(),
        Some(&CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Released { traveled: true }
        })
    );
}

#[test]
fn hold_deadzone_prefers_cached_sensor_dpi_over_the_armed_plan() {
    // 50 counts is a drag at 400 DPI (threshold ≈ 39) and a click at 1000
    // (threshold ≈ 98). The cycle write must win, or the deadzone drifts.
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0x0d02,
    };
    crate::remember_sensor_dpi(&route, Dpi::new(400));
    let planned = Some(Dpi::new(1000));
    assert_eq!(
        live_hold_dpi(&route, planned),
        Some(Dpi::new(400)),
        "a DPI-cycle write must size the next press, not the stale plan"
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let sets = ReprogSets {
        gesture_cids: &[],
        dpi_cids: &[],
        gesture_button_cids: &[],
        button_cids: &[],
        hold_button_cids: HOLD_BACK,
        sensor_dpi: live_hold_dpi(&route, planned),
    };
    handle_reprog_sets(&mut acc, hold_down(), &sets, &tx);
    handle_reprog_sets(
        &mut acc,
        RawControlEvent::RawXy {
            dx: 9_000,
            dy: 9_000,
        },
        &sets,
        &tx,
    );
    handle_reprog_sets(
        &mut acc,
        RawControlEvent::RawXy { dx: 50, dy: 0 },
        &sets,
        &tx,
    );
    handle_reprog_sets(&mut acc, release(), &sets, &tx);
    assert_eq!(
        drain(&mut rx).last(),
        Some(&CapturedInput::HoldEnd {
            button: ButtonId::Back,
            release: HoldRelease::Released { traveled: true }
        }),
        "50 counts at the cached 400 DPI must clear 2.5 mm; the planned 1000 would not"
    );
}

#[test]
fn the_pre_press_backlog_never_reaches_a_hold_mode_stream() {
    // Captured on an MX Master 3S at 950 DPI: the first report of a hold
    // arrived 10 ms after button-down carrying 1694 x 1619 counts, which is
    // 63 mm of travel. Delivered, it scrolled a 1080p view most of a screen
    // and marked a press that never moved as a drag.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_hold(&mut acc, hold_down(), &tx);
    handle_hold(
        &mut acc,
        RawControlEvent::RawXy {
            dx: -1694,
            dy: 1619,
        },
        &tx,
    );
    handle_hold(&mut acc, release(), &tx);

    assert_eq!(
        drain(&mut rx),
        vec![
            CapturedInput::HoldBegin(ButtonId::Back),
            CapturedInput::HoldEnd {
                button: ButtonId::Back,
                release: HoldRelease::Released { traveled: false }
            },
        ],
        "backlog banked before the press must neither pan nor count as travel"
    );
}

#[test]
fn every_press_drops_its_own_backlog_report() {
    // The divert re-arms per press, so the drop is per hold, not once per
    // capture session.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    for _ in 0..3 {
        handle_hold(&mut acc, hold_down(), &tx);
        handle_hold(
            &mut acc,
            RawControlEvent::RawXy {
                dx: -1694,
                dy: 1619,
            },
            &tx,
        );
        handle_hold(&mut acc, RawControlEvent::RawXy { dx: 4, dy: 3 }, &tx);
        handle_hold(&mut acc, release(), &tx);
    }

    let motion: Vec<_> = drain(&mut rx)
        .into_iter()
        .filter(|input| matches!(input, CapturedInput::HoldMotion { .. }))
        .collect();
    assert_eq!(
        motion,
        vec![
            CapturedInput::HoldMotion {
                button: ButtonId::Back,
                dx: 4,
                dy: 3
            };
            3
        ],
        "each press must flush a fresh backlog and stream only real travel"
    );
}

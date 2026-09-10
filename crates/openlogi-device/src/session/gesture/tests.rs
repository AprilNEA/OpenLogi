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

/// Feed one raw-XY report into a hold on `cids`.
fn raw_xy(
    acc: &mut CaptureAccum,
    cids: &[u16],
    dx: i16,
    dy: i16,
    tx: &mpsc::UnboundedSender<CapturedInput>,
) {
    handle_reprog(acc, RawControlEvent::RawXy { dx, dy }, cids, &[], &[], tx);
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
    // The taken-over hold is fresh, so its first raw-XY report is discarded as
    // a contact jump before the real swipe is read.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -3000, dy: 40 },
        BOTH,
        &[],
        &[],
        &tx,
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
    // The hold's first raw-XY report is discarded as a contact jump.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 8, dy: 1 },
        GESTURE,
        &[],
        &[],
        &tx,
    );
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
fn the_first_raw_xy_sample_after_contact_is_discarded() {
    // Real-hardware probe finding: a gesture source's first raw-XY sample after
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
fn the_dedicated_buttons_contact_jump_is_discarded_too() {
    // Real-hardware observation on the MX Master 4: the contact jump is
    // reported under the mechanical gesture button's CID as often as the haptic
    // panel's — raw-XY reports carry no CID on the wire, so the discard is not
    // scoped per control.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), GESTURE, &[], &[], &tx);
    acc.backdate_hold_for_test();
    // The dedicated button's contact jump — rightward, far past every threshold.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 4200, dy: 30 },
        GESTURE,
        &[],
        &[],
        &tx,
    );
    assert!(
        next_gesture(&mut rx).is_err(),
        "the dedicated button's contact jump must not commit a swipe"
    );
    // A leftward swipe follows from a clean accumulator: had the rightward jump
    // been summed, this could never commit Left.
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -120, dy: 5 },
        GESTURE,
        &[],
        &[],
        &tx,
    );
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Left
        ))
    );
}

#[test]
fn a_jump_several_reports_into_a_hold_is_discarded_and_motion_resumes() {
    // Real-hardware finding: the absolute-position artifact is not only a
    // contact jump — it also lands sporadically mid-hold, several clean reports
    // in. It must be dropped there too, and the motion on either side of it
    // must still resolve to the real direction.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), PANEL, &[], &[], &tx);
    acc.backdate_hold_for_test();
    raw_xy(&mut acc, PANEL, -2800, 30, &tx); // contact jump, discarded
    // A clean upward swipe builds over several small reports, none committing.
    raw_xy(&mut acc, PANEL, 3, -12, &tx);
    raw_xy(&mut acc, PANEL, 2, -14, &tx);
    raw_xy(&mut acc, PANEL, 4, -13, &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
        "a clean sub-threshold build must not commit yet"
    );
    // A mid-hold jump — rightward, into the thousands. Summed it would commit
    // Right on the spot; it must be discarded.
    raw_xy(&mut acc, PANEL, 9700, 40, &tx);
    assert!(
        next_gesture(&mut rx).is_err(),
        "the mid-hold jump must not commit a swipe"
    );
    // The upward motion continues from where it left off and commits Up.
    raw_xy(&mut acc, PANEL, 3, -18, &tx);
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Up
        )),
        "motion after the jump resolves to the real direction"
    );
}

#[test]
fn a_hold_whose_every_report_is_large_is_not_frozen_out() {
    // The bootstrap bound guards the reports before the hold has a per-report
    // scale. If every report is large — a high pointer resolution, or a very
    // fast swipe — that grace window must expire and let motion through rather
    // than filter the hold forever.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, panel_press(), PANEL, &[], &[], &tx);
    acc.backdate_hold_for_test();
    // Report 1 is the contact discard; the next few are held to the bootstrap
    // bound, then the grace window expires and motion accumulates.
    for _ in 0..6 {
        raw_xy(&mut acc, PANEL, 620, 20, &tx);
    }
    assert_eq!(
        next_gesture(&mut rx),
        Ok(CapturedInput::Gesture(
            ButtonId::HapticPanel,
            GestureDirection::Right
        )),
        "a hold of uniformly large reports still resolves"
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

// ── JumpFilter in isolation: the rules and their edges ───────────────────────

/// [`RAW_XY_JUMP_FLOOR`] as an `i16`, for building report values around it.
fn jump_floor() -> i16 {
    i16::try_from(RAW_XY_JUMP_FLOOR).unwrap()
}

#[test]
fn jump_filter_drops_a_contact_sources_first_report_always() {
    let mut f = JumpFilter::new(true);
    // Even a plausible small delta on report 1 is dropped — it is an absolute
    // position, not a delta.
    assert!(!f.admit(4, 2));
    // The next report is real motion.
    assert!(f.admit(4, 2));
}

#[test]
fn jump_filter_keeps_a_non_contact_sources_first_report() {
    let mut f = JumpFilter::new(false);
    // A repurposed standard button reports the relative pointer sensor from the
    // first sample.
    assert!(f.admit(30, 2));
}

#[test]
fn jump_filter_still_rejects_a_first_report_jump_from_a_non_contact_source() {
    let mut f = JumpFilter::new(false);
    // Nothing forces the first report out, but the opening bound still catches
    // an absolute position past ten whole swipes' worth of travel in one report.
    assert!(!f.admit(jump_floor() + 1, 0));
}

#[test]
fn jump_filter_drops_a_stray_mid_hold_jump() {
    let mut f = JumpFilter::new(false);
    assert!(f.admit(40, 3));
    assert!(f.admit(55, 4));
    // A single report far past the bound is the artifact; it is dropped and the
    // motion on either side keeps flowing.
    assert!(!f.admit(jump_floor() + 900, 40));
    assert!(f.admit(60, 5));
}

#[test]
fn jump_filter_bound_grows_with_the_hold_so_a_fast_swipe_is_never_filtered() {
    // High pointer resolution: real per-report travel climbs into the hundreds.
    // Each report a little bigger than the last — up to 5x the running peak —
    // must all be admitted, well past the opening floor.
    let mut f = JumpFilter::new(true);
    assert!(!f.admit(1500, 90)); // contact jump
    let mut prev = 30i16;
    for _ in 0..12 {
        let step = prev + prev / 2; // +50% per report
        assert!(
            f.admit(step, step / 8),
            "ramping step {step} must be admitted"
        );
        prev = step;
    }
    assert!(
        prev > jump_floor(),
        "the ramp climbed past the opening floor"
    );
}

#[test]
fn jump_filter_tiny_settling_reports_do_not_poison_the_bound() {
    // The failure that killed the mean-based design: contact jump, then a run of
    // ~1-unit settling reports, then real motion. The real motion must survive —
    // the peak only ever grows to a report's own size, so ~1-unit reports leave
    // it near zero and the opening floor still governs.
    let mut f = JumpFilter::new(true);
    assert!(!f.admit(3200, 180)); // contact jump
    for _ in 0..8 {
        assert!(f.admit(-1, 0)); // settling noise
    }
    for step in [-11, -28, -46, -75, -99, -60] {
        assert!(
            f.admit(step, -4),
            "real motion after settling must be admitted"
        );
    }
}

#[test]
fn jump_filter_disengages_when_every_report_trips_the_bound() {
    let mut f = JumpFilter::new(false);
    let big = jump_floor() / 3 * 4; // comfortably over the opening floor
    // A run of dropped reports means the bound is wrong for this device...
    for _ in 0..RAW_XY_JUMP_DISENGAGE_AFTER - 1 {
        assert!(!f.admit(big, 10));
    }
    // ...so the filter disengages and admits the rest of the hold.
    assert!(f.admit(big, 10));
    assert!(f.admit(big, 10));
}

#[test]
fn jump_filter_an_isolated_jump_never_disengages() {
    let mut f = JumpFilter::new(false);
    // Single jumps, each broken by real motion, must keep being caught.
    for _ in 0..6 {
        assert!(!f.admit(jump_floor() + 500, 30));
        assert!(f.admit(40, 3));
    }
    assert!(!f.admit(jump_floor() + 500, 30));
}

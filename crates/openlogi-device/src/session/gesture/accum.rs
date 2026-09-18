//! Sans-I/O accumulator for diverted `0x1b04` reports: which armed source
//! holds the raw-XY stream, and the button edges and gestures that follow.

use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use tokio::sync::mpsc;
use tracing::debug;

use super::{CapturedInput, GESTURE_SOURCE_BUTTONS};
use crate::reprog_controls::{self, RawControlEvent};

/// The hold that owns raw-XY motion, or the absence of one. Raw-XY reports
/// carry no source attribution, so the first held source owns the accumulated
/// motion until it is released (first hold wins); the per-hold qualifiers live
/// inside the variant so none can outlive the hold it belongs to.
#[derive(Default)]
enum HoldState {
    /// No armed gesture source is held: raw-XY reports are stray and dropped.
    #[default]
    Idle,
    /// `cid` began the current hold; its events dispatch as `button`. When the
    /// holder releases, a still-held source takes the hold over.
    Holding {
        /// The `0x1b04` control that owns this hold.
        cid: u16,
        /// The [`ButtonId`] the hold's gestures dispatch as.
        button: ButtonId,
        /// Mid-swipe travel accumulated over this hold.
        swipe: SwipeAccumulator,
        /// A second armed source is held alongside the holder. Overlap motion
        /// could belong to either control — dropped until the overlap ends.
        overlap: bool,
        /// The hold's next raw-XY sample must be dropped: the haptic panel's
        /// first sample after contact is an absolute position jump, not a
        /// delta (see [`reprog_controls::HAPTIC_PANEL_CID`]).
        skip_first_raw_xy: bool,
    },
}

/// Begin a hold for `cid`, its swipe accumulator started fresh.
fn begin_hold(cid: u16, button: ButtonId, overlap: bool, skip_first_raw_xy: bool) -> HoldState {
    let mut swipe = SwipeAccumulator::default();
    swipe.begin();
    HoldState::Holding {
        cid,
        button,
        swipe,
        overlap,
        skip_first_raw_xy,
    }
}

/// Movement + button state accumulated across messages. Lives behind a `Mutex`
/// because the channel's read thread invokes the listener by shared reference.
#[derive(Default)]
pub(super) struct CaptureAccum {
    /// The hold owning raw-XY motion, if any (see [`HoldState`]).
    hold: HoldState,
    /// The armed gesture sources held in the last event, for edge detection:
    /// a source not previously held that becomes the holder is a fresh touch
    /// (the haptic panel's first sample is then a contact jump to discard).
    gestures_down: Vec<u16>,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
    /// Diverted standard-button CIDs held in the last event.
    buttons_down: Vec<u16>,
}

#[cfg(test)]
impl CaptureAccum {
    /// Test-only seam mirroring [`SwipeAccumulator::backdate_hold_for_test`]
    /// for the current hold. A no-op while idle.
    pub(super) fn backdate_hold_for_test(&mut self) {
        if let HoldState::Holding { swipe, .. } = &mut self.hold {
            swipe.backdate_hold_for_test();
        }
    }
}

/// The [`ButtonId`] a gesture-source CID dispatches as, per
/// [`GESTURE_SOURCE_BUTTONS`]; `None` for a CID that is not a gesture source.
/// A spec listing an unknown CID therefore never begins a hold — the press is
/// dropped rather than misattributed.
fn gesture_source_button(cid: u16) -> Option<ButtonId> {
    GESTURE_SOURCE_BUTTONS
        .into_iter()
        .find(|&(c, _)| c == cid)
        .map(|(_, button)| button)
}

fn captured_gesture_button(cid: u16, gesture_button_cids: &[(u16, ButtonId)]) -> Option<ButtonId> {
    gesture_source_button(cid).or_else(|| {
        gesture_button_cids
            .iter()
            .find(|&&(candidate, _)| candidate == cid)
            .map(|&(_, button)| button)
    })
}

/// Update `acc` and emit on a decoded `0x1b04` event: preserve physical button
/// edges, and commit a gesture swipe the instant it crosses the threshold
/// (mid-swipe, like Options+) rather than on release.
pub(super) fn handle_reprog_with_gesture_buttons(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_cids: &[u16],
    dpi_cids: &[u16],
    gesture_button_cids: &[(u16, ButtonId)],
    button_cids: &[(u16, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            // The swipe accumulator belongs to the raw-XY gesture diverts.
            // When a gesture-source control is instead diverted as a plain
            // button (a single binding, not gesture mode), its press must flow
            // through the `button_cids` loop only — not also emit a click.
            let held: Vec<(u16, ButtonId)> = gesture_cids
                .iter()
                .filter(|cid| cids.contains(cid))
                .filter_map(|&cid| gesture_source_button(cid).map(|b| (cid, b)))
                .chain(
                    gesture_button_cids
                        .iter()
                        .copied()
                        .filter(|(cid, _)| cids.contains(cid)),
                )
                .collect();
            acc.hold = match std::mem::take(&mut acc.hold) {
                // The holder is still down. While a second armed source is
                // held alongside it, unattributed raw-XY motion is dropped
                // (see [`HoldState::Holding::overlap`]).
                HoldState::Holding {
                    cid,
                    button,
                    swipe,
                    skip_first_raw_xy,
                    ..
                } if cids.contains(&cid) => HoldState::Holding {
                    cid,
                    button,
                    swipe,
                    overlap: held.len() > 1,
                    skip_first_raw_xy,
                },
                previous => {
                    // No holder, or the holder released: a released hold that
                    // never committed a direction is a plain click...
                    if let HoldState::Holding {
                        button, mut swipe, ..
                    } = previous
                        && swipe.end()
                    {
                        debug!(%button, "gesture click");
                        let _ = sink.send(CapturedInput::Gesture(button, GestureDirection::Click));
                    }
                    // ...and the first still-held source begins (or takes
                    // over) the hold. A source not down in the previous event
                    // is a fresh touch, so the panel's contact-jump discard
                    // applies; one that was already held has had its jump
                    // dropped during the overlap.
                    match held.first() {
                        Some(&(cid, button)) => begin_hold(
                            cid,
                            button,
                            held.len() > 1,
                            cid == reprog_controls::HAPTIC_PANEL_CID
                                && !acc.gestures_down.contains(&cid),
                        ),
                        None => HoldState::Idle,
                    }
                }
            };
            // Gesture semantics stay separate from the physical lifecycle:
            // click/swipe remains one completed action, while every armed
            // source also contributes one rising and one falling edge to the
            // shared button runtime.
            for &cid in &acc.gestures_down {
                if !held.iter().any(|(held_cid, _)| *held_cid == cid)
                    && let Some(button) = captured_gesture_button(cid, gesture_button_cids)
                {
                    let _ = sink.send(CapturedInput::ButtonUp(button));
                }
            }
            for &(cid, button) in &held {
                if !acc.gestures_down.contains(&cid) {
                    let _ = sink.send(CapturedInput::ButtonDown(button));
                }
            }
            acc.gestures_down = held.into_iter().map(|(cid, _)| cid).collect();

            let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
            if dpi_down && !acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonDown(ButtonId::DpiToggle));
            } else if !dpi_down && acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonUp(ButtonId::DpiToggle));
            }
            acc.dpi_down = dpi_down;

            for &(cid, button) in button_cids {
                let down = cids.contains(&cid);
                let was_down = acc.buttons_down.contains(&cid);
                if down && !was_down {
                    let _ = sink.send(CapturedInput::ButtonDown(button));
                    acc.buttons_down.push(cid);
                } else if !down && was_down {
                    let _ = sink.send(CapturedInput::ButtonUp(button));
                    acc.buttons_down.retain(|&c| c != cid);
                }
            }
        }
        RawControlEvent::RawXy { dx, dy } => {
            handle_raw_xy(acc, dx, dy, sink);
        }
    }
}

fn handle_raw_xy(
    acc: &mut CaptureAccum,
    dx: i16,
    dy: i16,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    // Motion is attributed to the holding source; outside a hold the report
    // is stray and dropped.
    let HoldState::Holding {
        button,
        swipe,
        overlap,
        skip_first_raw_xy,
        ..
    } = &mut acc.hold
    else {
        return;
    };
    // While two armed sources are held the report could belong to either
    // control — drop it rather than miscommit a swipe through the holder's map.
    if *overlap {
        return;
    }
    // The haptic panel's first sample after contact is a position jump;
    // summing it would commit a bogus direction instantly.
    if *skip_first_raw_xy {
        *skip_first_raw_xy = false;
        return;
    }
    // Commit the instant a clean direction emerges (mid-swipe, once per hold);
    // the accumulator gates on hold duration internally and drops travel that
    // arrives outside a hold.
    if let Some(direction) = swipe.accumulate(i32::from(dx), i32::from(dy)) {
        debug!(?direction, %button, "gesture committed");
        let _ = sink.send(CapturedInput::Gesture(*button, direction));
    }
}

/// Test seam for the pre-existing raw-XY/plain-button cases, none of which
/// carries a standard-button gesture hold.
#[cfg(test)]
pub(super) fn handle_reprog(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_cids: &[u16],
    dpi_cids: &[u16],
    button_cids: &[(u16, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    handle_reprog_with_gesture_buttons(acc, event, gesture_cids, dpi_cids, &[], button_cids, sink);
}

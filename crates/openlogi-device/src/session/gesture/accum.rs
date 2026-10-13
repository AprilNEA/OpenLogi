//! Sans-I/O accumulator for diverted `0x1b04` reports: which armed source
//! holds the raw-XY stream, and the button edges and gestures that follow.

use std::collections::BTreeMap;
use std::time::Instant;

use openlogi_core::binding::{ButtonId, GestureDirection, GesturePolicy, SwipeAccumulator};
use tokio::sync::mpsc;
use tracing::debug;

use super::{CapturedInput, GESTURE_SOURCE_BUTTONS};
use crate::reprog_controls::RawControlEvent;

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
fn begin_hold(
    cid: u16,
    button: ButtonId,
    overlap: bool,
    skip_first_raw_xy: bool,
    policy: GesturePolicy,
    now: Instant,
) -> HoldState {
    let mut swipe = SwipeAccumulator::default();
    swipe.begin_at(now, policy);
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
    policies: BTreeMap<ButtonId, GesturePolicy>,
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

impl CaptureAccum {
    /// Retire physical input history when capture reconnects.
    pub(super) fn reset_input_state(&mut self) {
        let policies = std::mem::take(&mut self.policies);
        *self = Self::with_policies(policies);
    }

    pub(super) fn with_policies(policies: BTreeMap<ButtonId, GesturePolicy>) -> Self {
        Self {
            policies,
            ..Self::default()
        }
    }

    /// Update the accumulator and emit on a decoded `0x1b04` event: preserve
    /// physical button edges and emit each completed directional action.
    pub(super) fn on_event(
        &mut self,
        event: RawControlEvent,
        gesture_cids: &[u16],
        dpi_cids: &[u16],
        gesture_button_cids: &[(u16, ButtonId)],
        button_cids: &[(u16, ButtonId)],
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        self.on_event_at(
            Instant::now(),
            event,
            gesture_cids,
            dpi_cids,
            gesture_button_cids,
            button_cids,
            sink,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "existing report routing inputs plus explicit replay clock"
    )]
    pub(super) fn on_event_at(
        &mut self,
        now: Instant,
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
                self.hold = match std::mem::take(&mut self.hold) {
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
                            && swipe.end_at(now)
                        {
                            debug!(%button, "gesture click");
                            let _ =
                                sink.send(CapturedInput::Gesture(button, GestureDirection::Click));
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
                                !self.gestures_down.contains(&cid),
                                self.policies.get(&button).cloned().unwrap_or_default(),
                                now,
                            ),
                            None => HoldState::Idle,
                        }
                    }
                };
                // Gesture semantics stay separate from the physical lifecycle:
                // click/swipe remains one completed action, while every armed
                // source also contributes one rising and one falling edge to the
                // shared button runtime.
                for &cid in &self.gestures_down {
                    if !held.iter().any(|(held_cid, _)| *held_cid == cid)
                        && let Some(button) = captured_gesture_button(cid, gesture_button_cids)
                    {
                        let _ = sink.send(CapturedInput::ButtonUp(button));
                    }
                }
                for &(cid, button) in &held {
                    if !self.gestures_down.contains(&cid) {
                        let _ = sink.send(CapturedInput::ButtonDown(button));
                    }
                }
                self.gestures_down = held.into_iter().map(|(cid, _)| cid).collect();

                let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
                if dpi_down && !self.dpi_down {
                    let _ = sink.send(CapturedInput::ButtonDown(ButtonId::DpiToggle));
                } else if !dpi_down && self.dpi_down {
                    let _ = sink.send(CapturedInput::ButtonUp(ButtonId::DpiToggle));
                }
                self.dpi_down = dpi_down;

                for &(cid, button) in button_cids {
                    let down = cids.contains(&cid);
                    let was_down = self.buttons_down.contains(&cid);
                    if down && !was_down {
                        let _ = sink.send(CapturedInput::ButtonDown(button));
                        self.buttons_down.push(cid);
                    } else if !down && was_down {
                        let _ = sink.send(CapturedInput::ButtonUp(button));
                        self.buttons_down.retain(|&c| c != cid);
                    }
                }
            }
            RawControlEvent::RawXy { dx, dy } => {
                self.on_raw_xy(now, dx, dy, sink);
            }
        }
    }

    fn on_raw_xy(
        &mut self,
        now: Instant,
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
        } = &mut self.hold
        else {
            return;
        };
        // While two armed sources are held the report could belong to either
        // control — drop it rather than miscommit a swipe through the holder's map.
        if *overlap {
            return;
        }
        // A fresh hold's first report can contain travel from before the press
        // or a panel contact jump. Consume recognition's initial sample using
        // zero travel, preserving the stale-report rule for every source.
        if *skip_first_raw_xy {
            *skip_first_raw_xy = false;
            swipe.accumulate_at(now, 0, 0);
            return;
        }
        // Recognition and action thresholds require separate progress reports;
        // all actions remain owned by this same physical hold.
        if let Some(direction) = swipe.accumulate_at(now, i32::from(dx), i32::from(dy)) {
            debug!(?direction, %button, "gesture committed");
            let _ = sink.send(CapturedInput::Gesture(*button, direction));
        }
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
    acc.on_event(event, gesture_cids, dpi_cids, &[], button_cids, sink);
}

//! Sans-I/O accumulator for diverted `0x1b04` reports: which armed source
//! holds the raw-XY stream, the button edges and gestures that follow, and
//! which held wheel-hold button the raw wheel belongs to. While the capture
//! spec asks, the same hold also turns diverted main-wheel samples into
//! wheel directions.

use std::collections::BTreeMap;
use std::num::NonZeroU8;

use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use tokio::sync::mpsc;
use tracing::{debug, trace};

use super::{CapturedInput, GESTURE_SOURCE_BUTTONS, WheelNotchRepeat};
use crate::reprog_controls::{RawControlEvent, RawWheelEvent, RawWheelResolution};

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
        /// Main-wheel notches accumulated over this hold. Independent of
        /// `swipe`: a wheel notch must not look like a committed swipe.
        wheel: HoldWheel,
        /// A second armed source is held alongside the holder. Overlap motion
        /// could belong to either control — dropped until the overlap ends.
        overlap: bool,
        /// The hold's next raw-XY sample must be dropped: the first sample of
        /// a fresh hold is not travel made during it. It carries whatever the
        /// sensor accumulated since the previous raw-XY report — on an
        /// MX Master 3S, the hand's return stroke since the last gesture,
        /// thousands of units pointing the wrong way — and on the haptic
        /// panel a contact jump (see
        /// [`crate::reprog_controls::HAPTIC_PANEL_CID`]).
        skip_first_raw_xy: bool,
    },
}

/// Most gestures one wheel report may emit. A ratchet is one notch per
/// report. A coalesced high-resolution spin can be larger, and an `i16`
/// delta with a multiplier of 1 is 32767. Whole notches past the cap are
/// dropped, not saved for the next report.
const MAX_WHEEL_GESTURES_PER_REPORT: usize = 48;

/// Main-wheel progress for one gesture hold. `pending` is a signed fraction
/// of a notch: positive is away from the user ([`GestureDirection::WheelUp`]).
#[derive(Default)]
struct HoldWheel {
    /// Notch fraction still to be completed.
    pending: f64,
    /// This hold has already emitted [`GestureDirection::WheelUp`].
    up_sent: bool,
    /// This hold has already emitted [`GestureDirection::WheelDown`].
    down_sent: bool,
    /// A wheel gesture was emitted. The release then must not also click.
    committed: bool,
}

impl HoldWheel {
    /// Add one sample. `delta` uses the wheel's own sign (positive is away
    /// from the user) and is divided by `per_notch`, so one ratchet is one
    /// notch however many reports the firmware splits it into.
    ///
    /// A direction in `each` fires once per whole notch. Any other direction
    /// fires once for the hold; later notches that way are consumed and do
    /// not fire again, even after the wheel has been the other way. Whole
    /// notches past [`MAX_WHEEL_GESTURES_PER_REPORT`] are dropped. The
    /// sub-notch remainder stays, so reversing spends it before the other
    /// direction can complete. A sample that fires nothing does not set
    /// [`Self::committed`].
    fn observe(
        &mut self,
        delta: i16,
        per_notch: NonZeroU8,
        each: WheelNotchRepeat,
    ) -> Vec<GestureDirection> {
        let mut fired = Vec::new();
        self.pending += f64::from(delta) / f64::from(per_notch.get());
        if !self.pending.is_finite() {
            self.pending = 0.0;
            return fired;
        }
        while self.pending.abs() >= 1.0 {
            if fired.len() == MAX_WHEEL_GESTURES_PER_REPORT {
                self.pending = self.pending.fract();
                break;
            }
            let up = self.pending.is_sign_positive();
            let repeat = if up { each.up } else { each.down };
            let already = if up { self.up_sent } else { self.down_sent };
            if !repeat && already {
                self.pending = self.pending.fract();
                break;
            }
            if up {
                self.pending -= 1.0;
            } else {
                self.pending += 1.0;
            }
            if up {
                self.up_sent = true;
            } else {
                self.down_sent = true;
            }
            self.committed = true;
            fired.push(if up {
                GestureDirection::WheelUp
            } else {
                GestureDirection::WheelDown
            });
        }
        fired
    }
}

/// Begin a hold for `cid`, its swipe and wheel accumulators started fresh.
fn begin_hold(cid: u16, button: ButtonId, overlap: bool, skip_first_raw_xy: bool) -> HoldState {
    let mut swipe = SwipeAccumulator::default();
    swipe.begin();
    HoldState::Holding {
        cid,
        button,
        swipe,
        wheel: HoldWheel::default(),
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
    /// (its first raw-XY sample is then stale and discarded).
    gestures_down: Vec<u16>,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
    /// Diverted standard-button and wheel-hold CIDs held in the last event,
    /// in press order.
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

impl CaptureAccum {
    /// Update the accumulator and emit on a decoded `0x1b04` event: preserve
    /// physical button edges, and commit a gesture swipe the instant it
    /// crosses the threshold (mid-swipe, like Options+) rather than on release.
    pub(super) fn on_event(
        &mut self,
        event: RawControlEvent,
        gesture_cids: &[u16],
        dpi_cids: &[u16],
        gesture_button_cids: &[(u16, ButtonId)],
        button_cids: &[(u16, ButtonId)],
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        match event {
            RawControlEvent::DivertedButtons(cids) => {
                // The swipe accumulator belongs to the gesture-mode diverts:
                // raw-XY, or plain for a source without raw XY, whose holds
                // never move and so always end as a click.
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
                        wheel,
                        skip_first_raw_xy,
                        ..
                    } if cids.contains(&cid) => HoldState::Holding {
                        cid,
                        button,
                        swipe,
                        wheel,
                        overlap: held.len() > 1,
                        skip_first_raw_xy,
                    },
                    previous => {
                        // No holder, or the holder released: a released hold that
                        // never committed a swipe or a wheel notch is a plain click...
                        if let HoldState::Holding {
                            button,
                            mut swipe,
                            wheel,
                            ..
                        } = previous
                            && swipe.end()
                            && !wheel.committed
                        {
                            debug!(%button, "gesture click");
                            let _ =
                                sink.send(CapturedInput::Gesture(button, GestureDirection::Click));
                        }
                        // ...and the first still-held source begins (or takes
                        // over) the hold. A source not down in the previous event
                        // is a fresh touch, so its stale first sample is
                        // discarded; one that was already held has had that
                        // sample dropped during the overlap.
                        match held.first() {
                            Some(&(cid, button)) => begin_hold(
                                cid,
                                button,
                                held.len() > 1,
                                !self.gestures_down.contains(&cid),
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
                self.on_raw_xy(dx, dy, sink);
            }
        }
    }

    fn on_raw_xy(&mut self, dx: i16, dy: i16, sink: &mpsc::UnboundedSender<CapturedInput>) {
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
        trace!(dx, dy, %button, "gesture travel");
        // A fresh hold's first sample predates the hold; summing it would
        // commit a bogus direction instantly. It is spent whether or not an
        // overlap would have dropped it anyway.
        if *skip_first_raw_xy {
            *skip_first_raw_xy = false;
            return;
        }
        // While two armed sources are held the report could belong to either
        // control — drop it rather than miscommit a swipe through the holder's map.
        if *overlap {
            return;
        }
        // Commit the instant a clean direction emerges (mid-swipe, repeatable per hold);
        // the accumulator gates on hold duration internally and drops travel that
        // arrives outside a hold.
        if let Some(direction) = swipe.accumulate(i32::from(dx), i32::from(dy)) {
            debug!(?direction, %button, "gesture committed");
            let _ = sink.send(CapturedInput::Gesture(*button, direction));
        }
    }

    /// Attribute one raw wheel report to the earliest-pressed wheel-hold
    /// button still held. Raw wheel carries no source, and the firmware sends
    /// it only while such a control is held; a report with none held (a
    /// release that crossed it in flight) is dropped, as is a high-resolution
    /// report from a wheel whose multiplier is unknown, which cannot be
    /// scaled to notches.
    pub(super) fn on_raw_wheel(
        &self,
        event: RawWheelEvent,
        wheel_button_cids: &[(u16, ButtonId)],
        hires_per_notch: Option<NonZeroU8>,
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        if event.delta == 0 {
            return;
        }
        let Some(button) = self.buttons_down.iter().find_map(|&down| {
            wheel_button_cids
                .iter()
                .find(|&&(cid, _)| cid == down)
                .map(|&(_, button)| button)
        }) else {
            trace!(
                delta = event.delta,
                "raw wheel with no wheel-hold button held — dropped"
            );
            return;
        };
        let per_notch = match event.resolution {
            RawWheelResolution::Low => NonZeroU8::MIN,
            RawWheelResolution::High => {
                let Some(per_notch) = hires_per_notch else {
                    debug!(%button, "high-resolution raw wheel without a known multiplier — dropped");
                    return;
                };
                per_notch
            }
        };
        let _ = sink.send(CapturedInput::HeldWheel {
            button,
            delta: event.delta,
            per_notch,
        });
    }

    /// Attribute one diverted main-wheel sample to the gesture source that
    /// currently holds, when `enabled`.
    ///
    /// Returns `true` when the sample was consumed, so the caller must not
    /// also emit [`CapturedInput::MainWheel`]. A whole notch — `per_notch`
    /// units of `delta`, positive away from the user — emits
    /// [`CapturedInput::Gesture`] of [`GestureDirection::WheelUp`] or
    /// [`GestureDirection::WheelDown`], once per notch when `each_notch` says
    /// so and once per direction for the hold otherwise. The sample is still
    /// consumed when it is too small to fire or a one-shot direction has
    /// already fired, so the document under the pointer does not scroll.
    /// Returns `false`, emitting nothing, when the feature is off or no
    /// gesture source is held.
    ///
    /// Wheel samples carry their own feature index, unlike raw XY, so overlap
    /// does not drop them and the hold's first sample is not stale travel.
    #[must_use]
    pub(super) fn on_main_wheel(
        &mut self,
        delta: i16,
        per_notch: NonZeroU8,
        enabled: bool,
        each_notch: &BTreeMap<ButtonId, WheelNotchRepeat>,
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) -> bool {
        if !enabled {
            return false;
        }
        let HoldState::Holding { button, wheel, .. } = &mut self.hold else {
            return false;
        };
        let each = each_notch.get(button).copied().unwrap_or_default();
        for direction in wheel.observe(delta, per_notch, each) {
            debug!(?direction, %button, "gesture wheel notch");
            let _ = sink.send(CapturedInput::Gesture(*button, direction));
        }
        true
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

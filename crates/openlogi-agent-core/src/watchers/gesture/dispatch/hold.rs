//! Sans-I/O hold-mode driver: one live pan or zoom per capture session.
//!
//! Raw-XY arrives in sensor counts. Inject wants screen pixels (pan) or a
//! magnification increment (zoom). Both conversions are DPI-normalised so
//! felt speed does not change when the user cycles the sensor.

use std::collections::HashMap;

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::ZoomSensitivity;
use openlogi_core::hid::Dpi;
use openlogi_hid::HoldRelease;

use crate::capture_plan::DispatchPlan;
use crate::runtime::HidppSessionId;

/// Millimetres in one inch. DPI is counts per inch, so
/// `counts * `[`MM_PER_INCH`]` / dpi` is physical travel.
const MM_PER_INCH: f32 = 25.4;

/// Screen pixels of pan produced by one millimetre of mouse travel.
///
/// A 1080-pixel screen therefore takes `1080 / 22 ≈ 49 mm` of hand travel
/// (about two inches) at any sensor DPI. That is a short desktop swipe, not
/// a flick across the whole mousepad.
const PAN_PIXELS_PER_MM: f32 = 22.0;

/// Magnification increment per millimetre of vertical travel, at
/// [`ZoomSensitivity::DEFAULT`].
///
/// Twenty millimetres of travel accumulates `1.0`, which is a doubling of
/// the view. Dragging up (negative raw-XY `dy`) zooms in.
const ZOOM_MAGNIFICATION_PER_MM: f32 = 0.05;

/// Inject commands produced by one hold-mode transition. The dispatcher
/// applies these; tests assert the commands so they never post real events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum HoldCommand {
    PanBegin,
    Pan {
        dx: f32,
        dy: f32,
    },
    PanEnd,
    Zoom {
        amount: f32,
    },
    ZoomEnd,
    /// A Zoom-bound button clicked without dragging: fire the discrete
    /// native smart zoom instead of closing an empty pinch.
    SmartZoom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    Pan,
    Zoom,
}

impl HoldKind {
    fn from_action(action: &Action) -> Option<Self> {
        match action {
            Action::Pan => Some(Self::Pan),
            Action::Zoom => Some(Self::Zoom),
            _ => None,
        }
    }
}

/// Hold-mode feel settings, copied out of the dispatch plan when a hold
/// opens. Pan inversion and zoom scale must stay fixed for the life of one
/// gesture even if settings are adopted while the button is down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HoldFeel {
    zoom_sensitivity: ZoomSensitivity,
    invert_pan: bool,
}

/// Per-session slot. [`Self::Closed`] is terminal for that capture epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SessionHold {
    /// No hold yet this epoch; [`HoldSessions::begin`] is allowed.
    Idle,
    Open {
        button: ButtonId,
        kind: HoldKind,
        dpi: Dpi,
        /// Feel settings snapshotted at button-down, so a config refresh
        /// mid-gesture cannot change the scale under the user's hand.
        feel: HoldFeel,
    },
    /// Capture epoch was cancelled. Every later event is ignored.
    Closed,
}

/// Hold-mode state keyed by capture-session incarnation.
#[derive(Default)]
pub(super) struct HoldSessions {
    by_session: HashMap<HidppSessionId, SessionHold>,
}

impl HoldSessions {
    /// Open a hold if this epoch is still writable and `button` is bound.
    pub(super) fn begin(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        plan: &DispatchPlan,
    ) -> Option<HoldCommand> {
        if matches!(
            self.slot(session),
            SessionHold::Closed | SessionHold::Open { .. }
        ) {
            return None;
        }
        let kind = plan
            .hold_bindings
            .get(&button)
            .and_then(HoldKind::from_action)?;
        let dpi = plan.sensor_dpi.filter(|dpi| u16::from(*dpi) > 0)?;
        self.by_session.insert(
            session.clone(),
            SessionHold::Open {
                button,
                kind,
                dpi,
                feel: HoldFeel {
                    zoom_sensitivity: plan.zoom_sensitivity,
                    invert_pan: plan.invert_pan,
                },
            },
        );
        match kind {
            HoldKind::Pan => Some(HoldCommand::PanBegin),
            // Zoom opens on the first motion (`post_zoom_continuous` reopens
            // a pinch; begin must not emit a zero increment).
            HoldKind::Zoom => None,
        }
    }

    /// Stream one raw-XY report. Ignored unless this exact button is live.
    pub(super) fn motion(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        dx: i16,
        dy: i16,
    ) -> Option<HoldCommand> {
        let Some(SessionHold::Open {
            button: held,
            kind,
            dpi,
            feel,
        }) = self.by_session.get_mut(session)
        else {
            return None;
        };
        if *held != button {
            return None;
        }
        let (kind, dpi, feel) = (*kind, *dpi, *feel);
        match kind {
            HoldKind::Pan => {
                let (px, py) = pan_pixels(dx, dy, dpi);
                let (px, py) = if feel.invert_pan {
                    (-px, -py)
                } else {
                    (px, py)
                };
                (px != 0.0 || py != 0.0).then_some(HoldCommand::Pan { dx: px, dy: py })
            }
            HoldKind::Zoom => {
                let amount = zoom_magnification(dy, dpi, feel.zoom_sensitivity);
                (amount != 0.0).then_some(HoldCommand::Zoom { amount })
            }
        }
    }

    /// End the live hold. A late end after teardown or a completed hold
    /// does not emit.
    ///
    /// A Zoom hold the user released without clearing the physical click/drag
    /// deadzone is a click, and clicks fire the discrete smart zoom: the two
    /// gestures compose on one button, which is why smart zoom cannot fire on
    /// button-down. It has to wait and see whether a drag follows.
    ///
    /// [`HoldRelease::Interrupted`] is not a click, no matter how still the
    /// hold was. Capture interrupts a stream on reconnect, on teardown, and
    /// on the stale bound, all of them with the control still under the
    /// user's finger — a smart zoom there would fire into whatever happens to
    /// be frontmost, unasked.
    pub(super) fn end(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        release: HoldRelease,
    ) -> Option<HoldCommand> {
        let SessionHold::Open {
            button: held, kind, ..
        } = self.slot(session)
        else {
            return None;
        };
        if held != button {
            return None;
        }
        self.by_session.insert(session.clone(), SessionHold::Idle);
        Some(match (kind, release) {
            (HoldKind::Zoom, HoldRelease::Released { traveled: false }) => HoldCommand::SmartZoom,
            (kind, _) => end_command(kind),
        })
    }

    /// End a live hold but leave the epoch writable. A profile switch or
    /// dispatch-plan refresh must close inject without treating the next
    /// press as a late event on a dead session.
    pub(super) fn end_open(&mut self, session: &HidppSessionId) -> Option<HoldCommand> {
        let SessionHold::Open { kind, .. } = self.slot(session) else {
            return None;
        };
        self.by_session.insert(session.clone(), SessionHold::Idle);
        Some(end_command(kind))
    }

    /// Tear down any open hold and lock the epoch. Used for session Done,
    /// retirement, and stale input — a late begin must not start a hold
    /// nothing will close.
    pub(super) fn close_session(&mut self, session: &HidppSessionId) -> Option<HoldCommand> {
        let command = match self.slot(session) {
            SessionHold::Open { kind, .. } => Some(end_command(kind)),
            SessionHold::Idle | SessionHold::Closed => None,
        };
        self.by_session.insert(session.clone(), SessionHold::Closed);
        command
    }

    /// End every open hold and lock every known epoch (watcher / process exit).
    pub(super) fn close_all(&mut self) -> Vec<HoldCommand> {
        let sessions: Vec<_> = self.by_session.keys().cloned().collect();
        sessions
            .iter()
            .filter_map(|session| self.close_session(session))
            .collect()
    }

    fn slot(&self, session: &HidppSessionId) -> SessionHold {
        self.by_session
            .get(session)
            .copied()
            .unwrap_or(SessionHold::Idle)
    }
}

fn end_command(kind: HoldKind) -> HoldCommand {
    match kind {
        HoldKind::Pan => HoldCommand::PanEnd,
        HoldKind::Zoom => HoldCommand::ZoomEnd,
    }
}

/// Apply one command to the process-global inject sessions.
pub(super) fn emit(command: HoldCommand) {
    match command {
        HoldCommand::PanBegin => openlogi_inject::post_pan_begin(),
        HoldCommand::Pan { dx, dy } => openlogi_inject::post_pan(dx, dy),
        HoldCommand::PanEnd => openlogi_inject::post_pan_end(),
        HoldCommand::Zoom { amount } => openlogi_inject::post_zoom_continuous(amount),
        HoldCommand::ZoomEnd => openlogi_inject::post_zoom_end(),
        HoldCommand::SmartZoom => openlogi_inject::post_smart_zoom(),
    }
}

/// Close every inject session. Safe when none are open; required on
/// `process::exit` because that skips [`Drop`].
pub(super) fn flush_inject() {
    openlogi_inject::flush_gesture_sessions();
}

fn millimetres(counts: i16, dpi: Dpi) -> f32 {
    let dpi = f32::from(dpi);
    if dpi == 0.0 {
        return 0.0;
    }
    f32::from(counts) * MM_PER_INCH / dpi
}

fn pan_pixels(dx: i16, dy: i16, dpi: Dpi) -> (f32, f32) {
    (
        millimetres(dx, dpi) * PAN_PIXELS_PER_MM,
        millimetres(dy, dpi) * PAN_PIXELS_PER_MM,
    )
}

/// Positive amount zooms in. Raw-XY `+y` is down, so a negative `dy` (drag
/// toward the user) is a zoom-in.
fn zoom_magnification(dy: i16, dpi: Dpi, sensitivity: ZoomSensitivity) -> f32 {
    -millimetres(dy, dpi) * ZOOM_MAGNIFICATION_PER_MM * sensitivity.zoom_multiplier()
}

#[cfg(test)]
mod tests;

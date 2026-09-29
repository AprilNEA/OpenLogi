//! Synthetic magnification on macOS: the joined pinch gesture the wheel and the
//! one-shot zoom actions both drive.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use openlogi_core::binding::ZoomDirection;

use crate::inject::BUTTON_ZOOM_SOURCE;

use super::tag_synthetic;

// Undocumented gesture fields. There is no public API for synthesising a
// magnify gesture; these are the field numbers Mac Mouse Fix records from
// Calftrail's touch-synthesis reverse engineering.
const GESTURE_EVENT_TYPE: u32 = 55;
const GESTURE_HID_TYPE: u32 = 110;
const GESTURE_MAGNIFICATION: u32 = 113;
const GESTURE_PHASE: u32 = 132;
const NSEVENT_TYPE_GESTURE: i64 = 29;
const IOHID_EVENT_TYPE_ZOOM: i64 = 8;
// IOHIDEventPhaseBits, the same values `SmoothScrollPhase` already encodes.
const GESTURE_PHASE_BEGAN: i64 = 1;
const GESTURE_PHASE_CHANGED: i64 = 2;
const GESTURE_PHASE_ENDED: i64 = 4;

/// Fraction of current zoom per wheel notch. A trackpad pinch reports small
/// fractions per frame; this is one notch's worth.
const ZOOM_STEP: f64 = 0.05;

/// Idle gap that ends an open magnification gesture. Long enough to survive
/// the pause between two wheel increments at a slow turn, short enough that
/// the gesture does not outlive the user's intent.
const ZOOM_IDLE: Duration = Duration::from_millis(200);

static ZOOM_GESTURE: LazyLock<Mutex<ZoomGesture>> =
    LazyLock::new(|| Mutex::new(ZoomGesture::default()));

/// Zoom one notch from a button rather than the wheel.
///
/// A button press is a gesture of its own, so it borrows the same session with
/// one tick's worth under its own source name: it must not spend progress the
/// wheel made, nor leave any behind for the wheel to trip over.
pub(super) fn dispatch_zoom(direction: ZoomDirection) {
    let magnification = match direction {
        ZoomDirection::In => ZOOM_STEP,
        ZoomDirection::Out => -ZOOM_STEP,
    };
    post_zoom(magnification, BUTTON_ZOOM_SOURCE);
}

/// Post one zoom step as part of a continuous magnification (pinch) gesture.
///
/// ⌘+scroll is the Windows/Linux zoom convention and macOS does not share it:
/// AppKit and WebKit zoom on the trackpad's magnify gesture, so a browser
/// treats a ⌘-flagged wheel tick as plain scrolling. Canvas apps that read
/// `wheel` in their own code are the exception, which is why ⌘+scroll looks
/// like it works until it is tried on an ordinary page.
///
/// Steps are joined into one gesture rather than each being its own
/// begin/change/end. A browser's page zoom ignores a gesture that opens and
/// closes within a single step, and a wheel has no "fingers lifted" signal, so
/// the gesture is closed by [`ZOOM_IDLE`] of silence instead — see
/// [`ZoomGesture`].
///
/// The magnify event has no public constructor. These field numbers are the
/// ones Mac Mouse Fix documents from Calftrail's touch-synthesis work, and are
/// the only way to reach the gesture path other apps already listen on —
/// consistent with how this module's siblings reach Mission Control and Spaces
/// through private SPIs rather than synthesised chords.
pub(in crate::inject) fn post_zoom(magnification: f64, _source: &str) {
    // macOS needs no per-source accumulator: magnification is sent as a real
    // value inside a gesture session and the window server merges concurrent
    // sessions itself, so there is no fraction to strand between sources.
    let Ok(mut gesture) = ZOOM_GESTURE.lock() else {
        tracing::warn!("macOS zoom gesture mutex poisoned");
        return;
    };
    gesture.step(magnification);
}

/// Open/closed state of the synthetic pinch, plus whether its watchdog is
/// already waiting to close it.
#[derive(Default)]
struct ZoomGesture {
    /// `Some` while a gesture is open, carrying the last step's time.
    last_step: Option<Instant>,
    /// A watchdog thread is alive and will close the gesture.
    watchdog: bool,
}

impl ZoomGesture {
    /// Emit one step, opening the gesture first if it is not already open.
    fn step(&mut self, magnification: f64) {
        if self.last_step.is_none() {
            post_magnification(0.0, GESTURE_PHASE_BEGAN);
        }
        post_magnification(magnification, GESTURE_PHASE_CHANGED);
        self.last_step = Some(Instant::now());
        if !self.watchdog {
            self.watchdog = true;
            std::thread::spawn(close_when_idle);
        }
    }
}

/// Close the open gesture once the wheel has been quiet for [`ZOOM_IDLE`].
///
/// One thread serves the whole gesture: it re-checks the deadline rather than
/// being rescheduled per step, so a fast spin does not spawn a thread per
/// increment.
fn close_when_idle() {
    loop {
        std::thread::sleep(ZOOM_IDLE);
        let Ok(mut gesture) = ZOOM_GESTURE.lock() else {
            tracing::warn!("macOS zoom gesture mutex poisoned in watchdog");
            return;
        };
        let Some(last_step) = gesture.last_step else {
            gesture.watchdog = false;
            return;
        };
        if last_step.elapsed() >= ZOOM_IDLE {
            post_magnification(0.0, GESTURE_PHASE_ENDED);
            gesture.last_step = None;
            gesture.watchdog = false;
            return;
        }
    }
}

/// One magnification event at `phase`, carrying `magnification` as a fraction
/// of the current zoom (the trackpad's own unit).
fn post_magnification(magnification: f64, phase: i64) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for magnification");
        return;
    };
    let Ok(ev) = CGEvent::new(src) else {
        tracing::warn!("CGEvent::new failed for magnification");
        return;
    };
    ev.set_integer_value_field(GESTURE_EVENT_TYPE, NSEVENT_TYPE_GESTURE);
    ev.set_integer_value_field(GESTURE_HID_TYPE, IOHID_EVENT_TYPE_ZOOM);
    ev.set_integer_value_field(GESTURE_PHASE, phase);
    ev.set_double_value_field(GESTURE_MAGNIFICATION, magnification);
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

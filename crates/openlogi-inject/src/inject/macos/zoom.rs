//! Synthetic pinch-zoom gestures on macOS: discrete steps for button
//! bindings and a streamed pinch session for the thumb wheel.
//!
//! Trackpad pinches reach apps as a CGEvent whose *type* is 29
//! (NSEventTypeGesture — outside the typed CGEventType enum, hence the raw
//! FFI setter below), carrying the HID subtype in field 110,
//! IOHIDEventPhaseBits in field 132, and the magnification encoded as the
//! **bit pattern of an f32** written through the integer setters into fields
//! 115/117. An f64 in field 113 never reaches apps — this wire format comes
//! from CalfTrail Touch's trackpad-traffic reverse engineering and is
//! validated across AppKit/iWork apps by AbnormalMouseApp's
//! `EmulateEventPoster`, which this port follows event-for-event.
//!
//! This is why zoom isn't synthesised as Command+scroll-wheel like on the
//! other platforms: iWork/Preview-class apps implement zoom solely behind the
//! magnify gesture and ignore modifier-stamped scroll wheels entirely.

use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use super::tag_synthetic;

// ── Pinch-zoom gesture synthesis ───────────────────────────────────────────
// Trackpad pinches reach apps as a CGEvent whose *type* is 29
// (NSEventTypeGesture — outside the typed [`CGEventType`] enum, hence the raw
// FFI setter below), carrying the HID subtype in field 110,
// IOHIDEventPhaseBits in field 132, and the magnification encoded as the
// **bit pattern of an f32** written through the integer setters into fields
// 115/117. An f64 in field 113 never reaches apps — this wire format comes
// from CalfTrail Touch's trackpad-traffic reverse engineering and is
// validated across AppKit/iWork apps by AbnormalMouseApp's
// `EmulateEventPoster`, which this port follows event-for-event.
//
// This is why zoom isn't synthesised as ⌘+scroll-wheel like on the other
// platforms: iWork/Preview-class apps implement zoom solely behind the
// magnify gesture and ignore modifier-stamped scroll wheels entirely.

#[expect(
    unsafe_code,
    reason = "CGEventSetType is a plain CoreGraphics C function; type 29 lies outside the typed CGEventType enum"
)]
unsafe extern "C" {
    fn CGEventSetType(event: *mut std::ffi::c_void, event_type: u32);
}

/// CGEvent type for gesture events (`NSEventTypeGesture`).
const ZOOM_GESTURE_TYPE: u32 = 29;
/// Magnification delivered per wheel-line of rotation — matches the feel of
/// the verified prototype (t=100 over AbnormalMouse's base of 1000).
const ZOOM_MAGNIFICATION_PER_STEP: f64 = 0.1;

/// Gesture-layout field numbers (same reverse engineering as above).
const FIELD_SUBTYPE: u32 = 110;
const FIELD_ZOOM_DIR: u32 = 115;
const FIELD_SWIPE_DIR: u32 = 117;
const FIELD_PHASE: u32 = 132;
const FIELD_TRANSLATION_VALUE: u32 = 134;
const SUBTYPE_ZOOM: i64 = 8;
const SUBTYPE_TRANSLATION: i64 = 4;
const PHASE_BEGAN: i64 = 0x1;
const PHASE_CHANGED: i64 = 0x2;
const PHASE_ENDED: i64 = 0x4;
/// f32 bit pattern of 1.0 — the translation event's constant payload.
const TRANSLATION_ONE: i64 = 1_065_353_216;

/// Quiet period after which the streamed pinch of [`post_zoom_continuous`]
/// counts as finished and gets its closing `ended` pair.
const ZOOM_SESSION_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
/// Closer-thread polling cadence.
const ZOOM_SESSION_IDLE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Encode a magnification delta the way the gesture wire format expects: the
/// bit pattern of an f32 clamped to ±1 (AbnormalMouse divides by its base of
/// 1000; our per-line constant folds that scaling in up front).
#[expect(
    clippy::cast_possible_truncation,
    reason = "the gesture wire format carries an f32 bit pattern; the clamped product narrows deliberately"
)]
fn zoom_bits(magnification: f64) -> i64 {
    i64::from((magnification.clamp(-1.0, 1.0) as f32).to_bits())
}

/// Post one gesture-layout CGEvent (`subtype`, `phase`, payload `value`) —
/// the shared writer behind both zoom entry points.
#[expect(unsafe_code, reason = "calls the raw CGEventSetType declared above")]
fn post_gesture_event(src: &CGEventSource, subtype: i64, phase: i64, value: i64) {
    use foreign_types::ForeignType as _;

    let Ok(ev) = CGEvent::new(src.clone()) else {
        tracing::warn!("CGEvent::new failed for zoom gesture");
        return;
    };
    // SAFETY: passes the live CFTypeRef to the documented setter.
    unsafe { CGEventSetType(ev.as_ptr().cast(), ZOOM_GESTURE_TYPE) };
    ev.set_integer_value_field(FIELD_SUBTYPE, subtype);
    ev.set_integer_value_field(FIELD_PHASE, phase);
    if subtype == SUBTYPE_ZOOM {
        ev.set_integer_value_field(FIELD_ZOOM_DIR, value);
        ev.set_integer_value_field(FIELD_SWIPE_DIR, value);
    } else {
        ev.set_integer_value_field(FIELD_TRANSLATION_VALUE, value);
    }
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

/// Post one pinch-zoom step of `delta` wheel detents from a BUTTON press.
///
/// A press is inherently a discrete gesture — but only while no wheel-driven
/// pinch session is open. An independent began→ended pair posted mid-session
/// would end the app's active gesture while our own [`ZOOM_SESSION`] still
/// reads open, and the next wheel delta would land as an orphaned `changed`
/// (apps ignore it; zoom freezes until the idle closer resets us). So while a
/// session is open the step folds in as one more `changed` delta; with none,
/// it posts the standalone began→ended micro-gesture.
pub(super) fn post_zoom(delta: i32) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for zoom");
        return;
    };

    let bits = zoom_bits(f64::from(delta) * ZOOM_MAGNIFICATION_PER_STEP);
    let mut session = lock_zoom_session();
    if session.open {
        // The open wheel pinch owns the gesture channel: contribute the step
        // to it instead of competing with it.
        post_gesture_event(&src, SUBTYPE_ZOOM, PHASE_CHANGED, bits);
        post_gesture_event(&src, SUBTYPE_TRANSLATION, PHASE_CHANGED, TRANSLATION_ONE);
        session.last_activity = Some(std::time::Instant::now());
        return;
    }
    post_gesture_event(&src, SUBTYPE_ZOOM, PHASE_BEGAN, bits);
    post_gesture_event(&src, SUBTYPE_TRANSLATION, PHASE_BEGAN, TRANSLATION_ONE);
    post_gesture_event(&src, SUBTYPE_ZOOM, PHASE_ENDED, 0);
    post_gesture_event(&src, SUBTYPE_TRANSLATION, PHASE_ENDED, TRANSLATION_ONE);
}

/// Live state of the streamed pinch behind [`post_zoom_continuous`].
#[derive(Default)]
struct ZoomSession {
    open: bool,
    last_activity: Option<std::time::Instant>,
}

static ZOOM_SESSION: std::sync::LazyLock<std::sync::Mutex<ZoomSession>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(ZoomSession::default()));

/// Detached poller giving the streamed pinch its closing `ended` pair once
/// the thumb wheel goes quiet — the lifecycle beat AbnormalMouse gets from
/// key-release, which a free-spinning wheel never sends on its own.
static ZOOM_SESSION_CLOSER: std::sync::LazyLock<()> = std::sync::LazyLock::new(|| {
    if std::thread::Builder::new()
        .name("zoom-session-closer".into())
        .spawn(|| {
            loop {
                std::thread::sleep(ZOOM_SESSION_IDLE_POLL);
                close_zoom_session_if_idle();
            }
        })
        .is_err()
    {
        tracing::warn!("could not spawn zoom-session closer; pinches may stay open");
    }
});

fn lock_zoom_session() -> std::sync::MutexGuard<'static, ZoomSession> {
    ZOOM_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn close_zoom_session_if_idle() {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for zoom session end");
        return;
    };
    // Hold the lock across check-and-close so a delta arriving mid-close
    // cannot slip a `changed` in between the decision and the `ended`.
    let mut session = lock_zoom_session();
    if !(session.open
        && session
            .last_activity
            .is_some_and(|at| at.elapsed() >= ZOOM_SESSION_IDLE))
    {
        return;
    }
    post_gesture_event(&src, SUBTYPE_ZOOM, PHASE_ENDED, 0);
    post_gesture_event(&src, SUBTYPE_TRANSLATION, PHASE_ENDED, TRANSLATION_ONE);
    session.open = false;
    session.last_activity = None;
}

/// Stream one fractional wheel-lines increment of pinch-zoom into the focused
/// app — the thumb-wheel flavour of zoom. Every rotation report becomes a
/// `changed` event inside ONE ongoing gesture (began on the first delta after
/// a quiet spell, ended by [`ZOOM_SESSION_CLOSER`] once the wheel rests): the
/// same began→changed…→ended lifecycle as AbnormalMouseApp's
/// ZoomAndRotateController and a physical trackpad pinch, which is what makes
/// the zoom read as continuous instead of stepping per detent. Aggregate gain
/// is unchanged: `lines` arrives sensitivity-scaled from the watcher and
/// [`ZOOM_MAGNIFICATION_PER_STEP`] converts lines to magnification.
pub(super) fn post_zoom_continuous(lines: f32) {
    std::sync::LazyLock::force(&ZOOM_SESSION_CLOSER);

    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for continuous zoom");
        return;
    };

    let mut session = lock_zoom_session();
    let phase = if session.open {
        PHASE_CHANGED
    } else {
        PHASE_BEGAN
    };
    let bits = zoom_bits(f64::from(lines) * ZOOM_MAGNIFICATION_PER_STEP);

    post_gesture_event(&src, SUBTYPE_ZOOM, phase, bits);
    post_gesture_event(&src, SUBTYPE_TRANSLATION, phase, TRANSLATION_ONE);
    session.open = true;
    session.last_activity = Some(std::time::Instant::now());
}


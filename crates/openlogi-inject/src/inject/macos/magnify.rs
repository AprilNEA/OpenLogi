#![expect(
    unsafe_code,
    reason = "the CGEvent field setters for gesture synthesis are only reachable via CoreGraphics FFI"
)]
//! Field-based magnify (pinch-zoom) synthesis — the CalfTrail Touch /
//! Mac Mouse Fix recipe: a type-29 `NSEventTypeGesture` CGEvent carrying
//! the zoom subtype, the raw IOHIDEvent phase, and a relative scale delta.
//! Unlike DockSwipe, macOS 27 still honors these plain fields, so no
//! SkyLight bridge and no IOHIDEvent attachment are involved.

use std::ffi::{c_uint, c_void};

use crate::inject::GesturePhase;

/// CGEventField numbers the gesture carrier reads: the IOHIDEvent subtype,
/// the zoom amount, and the IOHIDEvent phase.
const FIELD_SUBTYPE: i32 = 110;
const FIELD_MAGNIFICATION: i32 = 113;
const FIELD_PHASE: i32 = 132;

/// `kIOHIDEventTypeZoom` on the subtype field.
const HID_TYPE_ZOOM: i64 = 8;

/// NSEventTypeGesture, the carrier for field-based gesture synthesis.
const GESTURE_CG_EVENT_TYPE: c_uint = 29;

/// kCGHIDEventTap.
const HID_EVENT_TAP: c_uint = 0;

/// The raw `kIOHIDEventPhase*` bits field 132 reads — unshifted, unlike the
/// DockSwipe options encoding.
fn phase_bits(phase: GesturePhase) -> i64 {
    match phase {
        GesturePhase::Began => 1,
        GesturePhase::Changed => 2,
        GesturePhase::End => 4,
        GesturePhase::Cancel => 8,
    }
}

pub(in crate::inject) fn post(phase: GesturePhase, magnification: f64) -> bool {
    // SAFETY: CGEventCreate(NULL) returns a +1 CGEventRef balanced by the
    // CFRelease at the end of this function.
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return false;
    }
    // SAFETY: event is a live +1 CGEventRef for the duration of the calls.
    unsafe {
        CGEventSetType(event, GESTURE_CG_EVENT_TYPE);
        CGEventSetIntegerValueField(event, FIELD_SUBTYPE, HID_TYPE_ZOOM);
        CGEventSetIntegerValueField(event, FIELD_PHASE, phase_bits(phase));
        CGEventSetDoubleValueField(event, FIELD_MAGNIFICATION, magnification);
        CGEventPost(HID_EVENT_TAP, event);
        CFRelease(event);
    }
    tracing::debug!(?phase, magnification, "magnify posted");
    true
}

#[cfg(test)]
mod tests {
    use super::phase_bits;
    use crate::inject::GesturePhase;

    #[test]
    fn phase_bits_match_iohid_event_phases() {
        assert_eq!(phase_bits(GesturePhase::Began), 1);
        assert_eq!(phase_bits(GesturePhase::Changed), 2);
        assert_eq!(phase_bits(GesturePhase::End), 4);
        assert_eq!(phase_bits(GesturePhase::Cancel), 8);
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreate(source: *const c_void) -> *const c_void;
    fn CGEventSetType(event: *const c_void, event_type: c_uint);
    fn CGEventSetIntegerValueField(event: *const c_void, field: i32, value: i64);
    fn CGEventSetDoubleValueField(event: *const c_void, field: i32, value: f64);
    fn CGEventPost(tap: c_uint, event: *const c_void);
    fn CFRelease(cf: *const c_void);
}

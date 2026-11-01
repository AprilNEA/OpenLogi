//! Smart Zoom: the trackpad's two-finger double-tap.
//!
//! WindowServer delivers that gesture to the app under the pointer as a
//! private gesture event, and apps that implement it (Safari, Preview,
//! Photos, ...) zoom the content there. The event is built the way the
//! Dock-swipe events in `spaces.rs` are: a generic event retyped to the
//! gesture type, with its gesture kind in the private subtype field.

use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType};

/// The generic gesture event type (`NSEventTypeGesture`).
const GESTURE_EVENT_TYPE: u32 = 29;
/// Private subtype field of a gesture event, the one the Dock swipes carry
/// their kind in.
const GESTURE_SUBTYPE_FIELD: u32 = 110;
/// The two-finger double-tap ("smart zoom") gesture kind.
const SMART_ZOOM_SUBTYPE: i64 = 22;

/// Post one Smart Zoom gesture at the current pointer location.
pub(super) fn smart_zoom() {
    let Some(event) = CGEvent::new(None) else {
        tracing::warn!("Smart Zoom: could not allocate the gesture event");
        return;
    };
    CGEvent::set_type(Some(&event), CGEventType(GESTURE_EVENT_TYPE));
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField(GESTURE_SUBTYPE_FIELD),
        SMART_ZOOM_SUBTYPE,
    );
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::EventSourceUserData,
        super::super::SYNTHETIC_EVENT_USER_DATA,
    );
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
}

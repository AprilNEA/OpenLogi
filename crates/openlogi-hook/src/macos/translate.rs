//! Translation of a tapped `CGEvent` into the crate's [`MouseEvent`] and
//! [`KeyEvent`] vocabulary: buttons, scroll deltas in the three encodings
//! macOS attaches to an axis, pointer deltas, and modifier flags.
//!
//! This runs inside the tap callback. Device identity comes from
//! [`super::sender`], which caches its registry walk per device.

use std::cell::RefCell;

use core_graphics::event::{CGEvent, CGEventField, CGEventFlags, CGEventType, EventField};
use tracing::debug;

use super::sender::{event_sender_id, sender_device_info};
use crate::{ButtonId, KeyEvent, KeyModifiers, MouseEvent, ScrollDelta};

/// Translate a raw OS button number to a [`ButtonId`].
///
/// Logi's convention: button 0 = left, 1 = right, 2 = middle, 3 = back,
/// 4 = forward. Numbers ≥5 don't map to a `ButtonId` we track.
fn button_number_to_id(n: i64) -> Option<ButtonId> {
    match n {
        0 => Some(ButtonId::LeftClick),
        1 => Some(ButtonId::RightClick),
        2 => Some(ButtonId::MiddleClick),
        3 => Some(ButtonId::Back),
        4 => Some(ButtonId::Forward),
        _ => None,
    }
}

thread_local! {
    /// Last successful device attribution, keyed by the HID sender id it came
    /// from.
    ///
    /// Some devices produce `CGEvent`s with no backing `IOHIDEvent` for a
    /// subset of their event types: a Bluetooth-direct MX Anywhere 3 attributes
    /// its Left/Right clicks and pointer moves normally, but every
    /// `OtherMouseDown`/`OtherMouseUp` (middle, back, forward) arrives with
    /// `CGEventCopyIOHIDEvent` returning null. Those events would otherwise be
    /// unattributable, and the remap policy fails closed on macOS — so the
    /// middle button silently does nothing.
    ///
    /// The attribution does not expire: a mouse left still for any length of
    /// time is still the device under the user's hand, and any other pointing
    /// device that moves replaces it, so the remap policy judges that device.
    static LAST_ATTRIBUTION: RefCell<Option<(u64, crate::EventDevice)>> =
        const { RefCell::new(None) };

    /// The identity each held button's press resolved to, so its release is
    /// judged as the same device even if another device moved in between.
    static HELD_SOURCES: RefCell<Vec<(ButtonId, Option<crate::EventDevice>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Record a successful attribution for later unattributed events.
fn remember_attribution(sender: u64, device: &crate::EventDevice) {
    LAST_ATTRIBUTION.with_borrow_mut(|slot| *slot = Some((sender, device.clone())));
}

/// The device the tap most recently attributed an event to.
fn last_attribution() -> Option<crate::EventDevice> {
    LAST_ATTRIBUTION.with_borrow(|slot| slot.as_ref().map(|(_, device)| device.clone()))
}

/// Refresh the remembered attribution from an event that has a HID backing.
/// Called from the pointer-move path so the fallback follows whichever device
/// last moved the pointer, even when no attributable button was clicked. The
/// device lookup only runs when the sender changes.
fn refresh_attribution(event: &CGEvent) {
    let Some(sender) = event_sender_id(event) else {
        return;
    };
    let unchanged =
        LAST_ATTRIBUTION.with_borrow(|slot| slot.as_ref().is_some_and(|(id, _)| *id == sender));
    if !unchanged {
        remember_attribution(sender, &sender_device_info(sender).event_device);
    }
}

/// Best-effort device identity for a button event's HID sender.
fn button_source(event: &CGEvent, id: ButtonId, pressed: bool) -> Option<crate::EventDevice> {
    let attributed = event_sender_id(event).map(|sender| {
        let device = sender_device_info(sender).event_device;
        remember_attribution(sender, &device);
        device
    });
    resolve_button_source(attributed, id, pressed)
}

/// Pick a button event's identity. With no backing IOHIDEvent, a press falls
/// back to the device the tap most recently attributed, and a release to the
/// identity its press resolved to, rather than being dropped as unattributable.
fn resolve_button_source(
    attributed: Option<crate::EventDevice>,
    id: ButtonId,
    pressed: bool,
) -> Option<crate::EventDevice> {
    HELD_SOURCES.with_borrow_mut(|held| {
        let press = held
            .iter()
            .position(|(held_id, _)| *held_id == id)
            .map(|index| held.swap_remove(index).1);
        if pressed {
            let device = attributed.or_else(last_attribution);
            held.push((id, device.clone()));
            device
        } else {
            attributed
                .or_else(|| press.flatten())
                .or_else(last_attribution)
        }
    })
}

/// Map the macOS modifier flags on a `CGEvent` to our [`KeyModifiers`].
/// `SecondaryFn` is deliberately ignored: it is firmware-internal and
/// unreliable as a trigger (function-key-remapper spec, Appendix A).
fn modifiers_from_flags(flags: CGEventFlags) -> KeyModifiers {
    KeyModifiers {
        shift: flags.contains(CGEventFlags::CGEventFlagShift),
        control: flags.contains(CGEventFlags::CGEventFlagControl),
        option: flags.contains(CGEventFlags::CGEventFlagAlternate),
        command: flags.contains(CGEventFlags::CGEventFlagCommand),
    }
}

/// Translate a keyboard `CGEvent` into a [`KeyEvent`]. Returns `None` for
/// non-key event types (the mouse path handles those), for `FlagsChanged`
/// (modifier state rides on the next key event via its flags; a standalone
/// flags change carries no key of interest to the remapper), and for a key
/// OpenLogi itself posted: a shortcut bound to a remapped key must not fire
/// that key's own binding, or two keys bound to each other would loop.
pub(super) fn translate_key(etype: CGEventType, event: &CGEvent) -> Option<KeyEvent> {
    let pressed = match etype {
        CGEventType::KeyDown => true,
        CGEventType::KeyUp => false,
        // FlagsChanged: no key to remap here.
        _ => return None,
    };
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
        == openlogi_inject::SYNTHETIC_EVENT_USER_DATA
    {
        return None;
    }
    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    let keycode = u16::try_from(keycode).ok()?;
    Some(KeyEvent {
        keycode,
        pressed,
        modifiers: modifiers_from_flags(event.get_flags()),
    })
}

/// Convert a `CGEvent` to our [`MouseEvent`] vocabulary. Returns `None`
/// for event types we don't translate (e.g. move events, unknown buttons).
pub(super) fn translate(etype: CGEventType, event: &CGEvent) -> Option<MouseEvent> {
    // Skip events OpenLogi itself synthesised, so a remapped click or inverted
    // scroll we posted doesn't re-enter the hook as real input. Gate the field
    // read to events we synthesize — keeping the FFI call off the high-rate
    // pointer-move stream.
    let can_be_synthetic = matches!(
        etype,
        CGEventType::LeftMouseDown
            | CGEventType::LeftMouseUp
            | CGEventType::RightMouseDown
            | CGEventType::RightMouseUp
            | CGEventType::OtherMouseDown
            | CGEventType::OtherMouseUp
            | CGEventType::ScrollWheel
    );
    if can_be_synthetic
        && event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
            == openlogi_inject::SYNTHETIC_EVENT_USER_DATA
    {
        return None;
    }
    match etype {
        CGEventType::LeftMouseDown => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: true,
            device: button_source(event, ButtonId::LeftClick, true),
        }),
        CGEventType::LeftMouseUp => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: false,
            device: button_source(event, ButtonId::LeftClick, false),
        }),
        CGEventType::RightMouseDown => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: true,
            device: button_source(event, ButtonId::RightClick, true),
        }),
        CGEventType::RightMouseUp => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: false,
            device: button_source(event, ButtonId::RightClick, false),
        }),
        CGEventType::OtherMouseDown => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button {
                id,
                pressed: true,
                device: button_source(event, id, true),
            })
        }
        CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button {
                id,
                pressed: false,
                device: button_source(event, id, false),
            })
        }
        CGEventType::ScrollWheel => {
            // axis 1 = vertical scroll; axis 2 = horizontal scroll. Continuous
            // events carry pixel-precise distance; non-continuous events carry
            // line distance, including fractional lines in the 16.16 fields.
            // Preserve that distinction instead of handing consumers an
            // unlabelled number that cannot be safely interpolated.
            let continuous =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0;
            let delta = if continuous {
                ScrollDelta::pixels(
                    precise_scroll_delta(event, HORIZONTAL),
                    precise_scroll_delta(event, VERTICAL),
                )
            } else {
                non_continuous_scroll_delta(event)
            };
            // Device identity is the reliable signal: a free-spinning Logitech
            // wheel sets the CGEvent phase, so phase alone misclassifies it as a
            // trackpad. Fall back to the phase heuristic only for a sender-less
            // (synthetic) event, which has no device to identify.
            let phase = event.get_integer_value_field(SCROLL_PHASE) != 0
                || event.get_integer_value_field(MOMENTUM_PHASE) != 0
                || event.get_integer_value_field(SCROLL_COUNT) != 0;
            let sender = event_sender_id(event);
            let device_info = sender.map(sender_device_info);
            let from_trackpad = device_info.as_ref().map_or(phase, |info| info.is_trackpad);
            Some(MouseEvent::Scroll {
                delta,
                from_trackpad,
                device: device_info.map(|info| info.event_device),
            })
        }
        // Pointer movement feeds gesture-button swipe detection. While a button
        // is physically held the OS reports *Dragged rather than MouseMoved, so
        // a gesture button's hold-and-swipe arrives here as OtherMouseDragged.
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            refresh_attribution(event);
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "per-event pointer deltas are small integers, far within i32"
            )]
            Some(MouseEvent::Moved {
                delta_x: dx as i32,
                delta_y: dy as i32,
            })
        }
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
            // The run-loop slice re-enables the tap (see `thread_main`); surface
            // the interruption so the runtime cancels any in-progress hold — a
            // button-up dropped during the gap must not later fire a phantom
            // swipe off ordinary cursor motion. Logged at debug, not warn:
            // TapDisabledByUserInput fires during ordinary heavy input bursts and
            // self-heals next slice, so it isn't worth a warning each time.
            debug!("CGEventTap disabled by OS (type={etype:?}); re-enabling, cancelling any hold");
            Some(MouseEvent::CaptureInterrupted)
        }
        _ => None,
    }
}

/// The three delta encodings macOS attaches to one scroll axis: the coarse
/// integer line delta, the fixed-point delta, and the pixel-precise point
/// delta. An app reads whichever it prefers, so any transform must touch all
/// three.
#[derive(Clone, Copy)]
pub(super) struct ScrollAxisFields {
    pub(super) line: CGEventField,
    pub(super) fixed: CGEventField,
    pub(super) point: CGEventField,
}

pub(super) const VERTICAL: ScrollAxisFields = ScrollAxisFields {
    line: EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
    fixed: EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
    point: EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
};
const HORIZONTAL: ScrollAxisFields = ScrollAxisFields {
    line: EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    fixed: EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
    point: EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
};

// Phase fields aren't exposed by core-graphics 0.25; the raw ids come from
// `CGEventTypes.h`. A trackpad sets one of these; a mouse wheel never does.
const SCROLL_PHASE: CGEventField = 99; // kCGScrollWheelEventScrollPhase
const SCROLL_COUNT: CGEventField = 100; // kCGScrollWheelEventScrollCount
const MOMENTUM_PHASE: CGEventField = 123; // kCGScrollWheelEventMomentumPhase

/// The pixel magnitude for continuous `axis`, preferring the point field and
/// falling back to the fixed-point field used by older producers.
fn precise_scroll_delta(event: &CGEvent, axis: ScrollAxisFields) -> f64 {
    let point = event.get_double_value_field(axis.point);
    if point != 0.0 {
        return point;
    }
    let fixed = event.get_double_value_field(axis.fixed);
    if fixed != 0.0 {
        return fixed;
    }
    0.0
}

/// Preserve fractional line distance from a non-continuous high-resolution
/// wheel. `CGEventGetDoubleValueField` decodes the signed 16.16 field for us;
/// the integer line field is only the fallback for older event producers.
///
/// A producer may expose only point distance. Apple defines no universal
/// point-to-line ratio, so retain that event as pixels instead of inventing a
/// wheel-tick conversion or dropping it as a zero-line event.
fn non_continuous_scroll_delta(event: &CGEvent) -> ScrollDelta {
    let x = fractional_line_scroll_delta(event, HORIZONTAL);
    let y = fractional_line_scroll_delta(event, VERTICAL);
    if x != 0.0 || y != 0.0 {
        return ScrollDelta::wheel_ticks(x, y);
    }

    ScrollDelta::pixels(
        event.get_double_value_field(HORIZONTAL.point),
        event.get_double_value_field(VERTICAL.point),
    )
}

fn fractional_line_scroll_delta(event: &CGEvent, axis: ScrollAxisFields) -> f64 {
    let fixed = event.get_double_value_field(axis.fixed);
    if fixed != 0.0 {
        return fixed;
    }
    line_scroll_delta(event, axis)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "physical per-event line deltas are small integers, exactly represented by f64"
)]
fn line_scroll_delta(event: &CGEvent, axis: ScrollAxisFields) -> f64 {
    event.get_integer_value_field(axis.line) as f64
}

#[cfg(test)]
mod tests {
    use core_graphics::event::{CGEvent, CGEventType, EventField};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::{
        LAST_ATTRIBUTION, last_attribution, remember_attribution, resolve_button_source,
        translate_key,
    };
    use crate::ButtonId;

    fn key_down(vk: u16) -> CGEvent {
        let source = CGEventSource::new(CGEventSourceStateID::Private).expect("an event source");
        CGEvent::new_keyboard_event(source, vk, true).expect("a keyboard event")
    }

    #[test]
    fn a_key_openlogi_posted_is_not_translated() {
        let event = key_down(0x7a);
        assert!(translate_key(CGEventType::KeyDown, &event).is_some());

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            openlogi_inject::SYNTHETIC_EVENT_USER_DATA,
        );
        assert!(translate_key(CGEventType::KeyDown, &event).is_none());
    }

    fn device(product_id: u32, name: &str) -> crate::EventDevice {
        crate::EventDevice {
            vendor_id: Some(crate::LOGITECH_VENDOR_ID),
            product_id: Some(product_id),
            product_name: Some(name.into()),
        }
    }

    /// An unattributed press falls back to the last attribution, so a device
    /// whose CGEvents carry no backing IOHIDEvent for some event types is not
    /// dropped by the fail-closed remap policy.
    #[test]
    fn last_attribution_backs_an_unattributed_press() {
        let mouse = device(0xb025, "MX Anywhere 3");
        LAST_ATTRIBUTION.with_borrow_mut(|slot| *slot = None);
        assert_eq!(resolve_button_source(None, ButtonId::Back, true), None);
        assert_eq!(resolve_button_source(None, ButtonId::Back, false), None);

        remember_attribution(1, &mouse);
        assert_eq!(last_attribution(), Some(mouse.clone()));
        assert_eq!(
            resolve_button_source(None, ButtonId::MiddleClick, true),
            Some(mouse)
        );
    }

    /// A release keeps its press's identity even when another device was
    /// attributed while the button was held.
    #[test]
    fn an_unattributed_release_keeps_its_press_identity() {
        let mouse = device(0xb025, "MX Anywhere 3");
        let other = device(0xb034, "MX Master 3S");
        remember_attribution(1, &mouse);
        assert_eq!(
            resolve_button_source(None, ButtonId::MiddleClick, true),
            Some(mouse.clone())
        );

        remember_attribution(2, &other);
        assert_eq!(
            resolve_button_source(None, ButtonId::MiddleClick, false),
            Some(mouse)
        );
        assert_eq!(
            resolve_button_source(None, ButtonId::MiddleClick, false),
            Some(other)
        );
    }
}

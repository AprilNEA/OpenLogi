//! macOS `CGEventTap` implementation of the OS-level mouse hook.
#![allow(
    unsafe_code,
    reason = "the event tap is built on Core Graphics / Core Foundation C APIs"
)]

mod watchdog;

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use core_foundation::base::{CFTypeRef, TCFType as _};
use core_foundation::number::CFNumber;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEvent, CGEventField, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use foreign_types_shared::ForeignType as _;
use objc2_application_services::{AXIsProcessTrusted, AXIsProcessTrustedWithOptions};
use tracing::{debug, error, warn};

use crate::{
    ButtonId, CursorPosition, EventDevice, EventDisposition, EventTapInfo, HookError, HookEvent,
    KeyEvent, KeyModifiers, MouseEvent, TapLocation,
};
use watchdog::{
    LifecycleDecision, LifecycleExitReason, LifecycleObservation, LifecycleWatchdog, TapPhase,
    WatchdogSignals, stuck_callback,
};

/// Everything `Hook` needs to control the background thread.
pub(crate) struct HookInner {
    thread: thread::JoinHandle<()>,
    lifecycle_watchdog: thread::JoinHandle<()>,
    run_loop: CFRunLoop,
    /// Lifecycle signals re-checked at the top of every run-loop slice.
    /// `run_loop.stop()` only interrupts the loop while it is *inside* a
    /// `run_in_mode` slice; a stop landing in the gap between slices is
    /// dropped, so the stop latch — not the CF stop alone — is the reliable
    /// shutdown signal. The independent lifecycle watchdog keeps observing
    /// that latch until the tap thread proves the tap is gone.
    signals: Arc<WatchdogSignals>,
}

// SAFETY: CFRunLoop is a Core Foundation ref-counted object. The CF
// documentation states that CFRunLoop objects can be passed between
// threads; only CFRunLoopRun must be called on the owning thread.
unsafe impl Send for HookInner {}

/// Opaque `IOHIDEventRef` — the HID event backing a `CGEvent`.
type IOHIDEventRef = *mut std::ffi::c_void;

// Device-of-origin lookup. `CGEventCopyIOHIDEvent` (CoreGraphics) returns the
// HID event behind a CGEvent; `IOHIDEventGetSenderID` (IOKit) yields the
// registry id of the producing service. These are undocumented but long-stable
// symbols (Mac Mouse Fix / Karabiner use them) — the only reliable way to tell a
// hi-res mouse wheel from a trackpad, which carry identical CGEvent phase flags.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCopyIOHIDEvent(event: *const std::ffi::c_void) -> IOHIDEventRef;
}
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventGetSenderID(event: IOHIDEventRef) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// The registry id of the device that produced `event`, via its backing
/// IOHIDEvent. `None` for events with no HID backing (e.g. synthetic ones).
fn event_sender_id(event: &CGEvent) -> Option<u64> {
    // SAFETY: `event.as_ptr()` is the live CGEventRef; `CGEventCopyIOHIDEvent`
    // returns a +1-retained IOHIDEvent (or null) which we release below.
    let hid = unsafe { CGEventCopyIOHIDEvent(event.as_ptr().cast()) };
    if hid.is_null() {
        return None;
    }
    // SAFETY: `hid` is a live IOHIDEvent for the duration of the call.
    let sender = unsafe { IOHIDEventGetSenderID(hid) };
    // SAFETY: balance the +1 retain from `CGEventCopyIOHIDEvent`.
    unsafe { CFRelease(hid) };
    Some(sender)
}

/// IOKit registry walk to read a device's HID usage page. `IORegistryEntryIDMatching`
/// builds a matching dict for the service id; `IOServiceGetMatchingService` resolves
/// it (and releases the dict); `IORegistryEntrySearchCFProperty` reads a property,
/// searching parents so the usage page on the owning `IOHIDDevice` is found.
type IoObjectT = u32;
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryEntryIDMatching(entry_id: u64) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *const std::ffi::c_void) -> IoObjectT;
    fn IORegistryEntrySearchCFProperty(
        entry: IoObjectT,
        plane: *const std::ffi::c_char,
        key: CFStringRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObjectT) -> i32;
}

const IO_REGISTRY_ITERATE_RECURSIVELY: u32 = 1;
const IO_REGISTRY_ITERATE_PARENTS: u32 = 2;

/// Resolve `sender_id` to its IO service, or `None`. Caller must
/// `IOObjectRelease` the result.
fn open_service(sender_id: u64) -> Option<IoObjectT> {
    // SAFETY: returns a +1 matching dict; `IOServiceGetMatchingService` consumes it.
    let matching = unsafe { IORegistryEntryIDMatching(sender_id) };
    if matching.is_null() {
        return None;
    }
    // SAFETY: `matching` is a valid +1 dict (consumed here); 0 = default main port.
    let service = unsafe { IOServiceGetMatchingService(0, matching) };
    (service != 0).then_some(service)
}

/// Read property `key` off `service` (searching parents), as a `+1` CFTypeRef the
/// caller owns. `None` if absent.
fn service_property(service: IoObjectT, key: &str) -> Option<CFTypeRef> {
    let cf_key = CFString::new(key);
    let plane = c"IOService";
    // SAFETY: `service` is live; `cf_key` is a valid CFStringRef; null allocator is
    // the documented default; returns a +1 CF value (or null).
    let prop = unsafe {
        IORegistryEntrySearchCFProperty(
            service,
            plane.as_ptr(),
            cf_key.as_concrete_TypeRef(),
            std::ptr::null(),
            IO_REGISTRY_ITERATE_RECURSIVELY | IO_REGISTRY_ITERATE_PARENTS,
        )
    };
    (!prop.is_null()).then_some(prop)
}

/// Cached device facts derived from the IOKit sender id behind a scroll event.
#[derive(Clone, Default)]
struct SenderDeviceInfo {
    event_device: EventDevice,
    is_trackpad: bool,
}

/// Device facts for the registry id `sender_id`. A trackpad presents a *mouse*
/// HID interface for scrolling, so usage can't separate it from a real wheel;
/// product identity stays stable across wheel modes, unlike CGEvent phase.
/// Cached per id because the registry walk is slow and identity never changes.
fn sender_device_info(sender_id: u64) -> SenderDeviceInfo {
    thread_local! {
        static CACHE: RefCell<HashMap<u64, SenderDeviceInfo>> = RefCell::new(HashMap::new());
    }
    CACHE.with_borrow_mut(|cache| {
        cache
            .entry(sender_id)
            .or_insert_with(|| {
                let Some(service) = open_service(sender_id) else {
                    return SenderDeviceInfo::default();
                };
                let string_prop = |k| {
                    // SAFETY: a String property is a +1 CFString; wrap takes ownership.
                    service_property(service, k)
                        .map(|p| unsafe { CFString::wrap_under_create_rule(p.cast()) }.to_string())
                };
                let num_prop = |k| {
                    service_property(service, k)
                        .and_then(|p| {
                            // SAFETY: `service_property` only yields a non-null, +1 CF
                            // value, and every key passed below is published by IOKit as
                            // a CFNumber, so the cast keeps the type; the create rule
                            // hands that retain to the wrapper, which releases it on drop.
                            unsafe { CFNumber::wrap_under_create_rule(p.cast()) }.to_i64()
                        })
                        .and_then(|n| u32::try_from(n).ok())
                };
                let product_name = string_prop("Product");
                let info = SenderDeviceInfo {
                    is_trackpad: product_name
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains("trackpad")),
                    event_device: EventDevice {
                        vendor_id: num_prop("VendorID").or_else(|| num_prop("idVendor")),
                        product_id: num_prop("ProductID").or_else(|| num_prop("idProduct")),
                        product_name,
                    },
                };
                // SAFETY: `service` is a live io_object_t we own.
                unsafe { IOObjectRelease(service) };
                info
            })
            .clone()
    })
}

/// Check whether this process has been granted Accessibility access.
pub(crate) fn has_accessibility() -> bool {
    // SAFETY: takes no arguments and only reads the current trust state — the
    // non-prompting counterpart of `AXIsProcessTrustedWithOptions`.
    unsafe { AXIsProcessTrusted() }
}

/// Raise the Accessibility prompt + register the process. See
/// [`super::Hook::prompt_accessibility`].
///
/// The `kAXTrustedCheckOptionPrompt = true` option is what makes macOS surface
/// the dialog and list the process in System Settings; without it this is just
/// [`has_accessibility`].
pub(crate) fn prompt_accessibility() {
    use objc2_application_services::kAXTrustedCheckOptionPrompt;
    use objc2_core_foundation::{CFDictionary, kCFBooleanTrue};

    // SAFETY: both are framework-provided constants, live for the process
    // lifetime; reading them copies a `&'static` reference.
    let (key, value) = unsafe { (kAXTrustedCheckOptionPrompt, kCFBooleanTrue) };
    let Some(value) = value else { return };
    let options = CFDictionary::from_slices(&[key], &[value]);
    // SAFETY: the dictionary holds exactly the documented key/value types
    // (`kAXTrustedCheckOptionPrompt` → `CFBoolean`). The returned trust state is
    // observed separately via the watcher, so it is deliberately dropped here.
    let _trusted = unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
}

/// Read the frontmost application's bundle identifier via `NSWorkspace`.
/// Returns `None` when no app is frontmost or the identifier is missing.
///
/// `NSWorkspace` is `AnyThread`, so this is sound on the watcher thread. The
/// reads return owned `Retained` values (no leak by construction), but the
/// framework still autoreleases internal temporaries and `to_str` borrows its
/// UTF-8 view from the pool — so an explicit `autoreleasepool` is required off
/// the main thread, where no run loop drains one. (Without it the old raw path
/// leaked the workspace/app/bundle-id objects: hundreds of MB across a workday.)
pub(crate) fn cursor_position() -> Option<CursorPosition> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let point = CGEvent::new(source).ok()?.location();
    Some(CursorPosition {
        x: point.x,
        y: point.y,
    })
}

pub(crate) fn frontmost_bundle_id() -> Option<String> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;

    autoreleasepool(|pool| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let bundle_id = app.bundleIdentifier()?;
        // SAFETY: `to_str` yields a UTF-8 view valid for `pool`'s lifetime; we
        // copy it into an owned `String` before the pool (and `bundle_id`) drop,
        // so the borrow never escapes.
        Some(unsafe { bundle_id.to_str(pool) }.to_owned())
    })
}

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

/// Best-effort device identity for a button event's HID sender.
fn button_source(event: &CGEvent) -> Option<crate::EventDevice> {
    event_sender_id(event).map(|id| sender_device_info(id).event_device)
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
/// non-key event types (the mouse path handles those) and for `FlagsChanged`
/// (modifier state rides on the next key event via its flags; a standalone
/// flags change carries no key of interest to the remapper).
fn translate_key(etype: CGEventType, event: &CGEvent) -> Option<KeyEvent> {
    let pressed = match etype {
        CGEventType::KeyDown => true,
        CGEventType::KeyUp => false,
        // FlagsChanged: no key to remap here.
        _ => return None,
    };
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
fn translate(etype: CGEventType, event: &CGEvent) -> Option<MouseEvent> {
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
            device: button_source(event),
        }),
        CGEventType::LeftMouseUp => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: false,
            device: button_source(event),
        }),
        CGEventType::RightMouseDown => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: true,
            device: button_source(event),
        }),
        CGEventType::RightMouseUp => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: false,
            device: button_source(event),
        }),
        CGEventType::OtherMouseDown => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button {
                id,
                pressed: true,
                device: button_source(event),
            })
        }
        CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button {
                id,
                pressed: false,
                device: button_source(event),
            })
        }
        CGEventType::ScrollWheel => {
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
            // axis 1 = vertical scroll; axis 2 = horizontal scroll. Read the
            // pixel-precise delta in preference to the coarse line delta (a hi-res
            // wheel reports its motion in the pixel field with the line field at 0,
            // so reading only the line field would look like "no scroll").
            let dy = usable_scroll_delta(event, VERTICAL);
            let dx = usable_scroll_delta(event, HORIZONTAL);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scroll deltas are small fractional values that fit comfortably in f32"
            )]
            Some(MouseEvent::Scroll {
                delta_x: dx as f32,
                delta_y: dy as f32,
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
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
            #[allow(
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
struct ScrollAxisFields {
    line: CGEventField,
    fixed: CGEventField,
    point: CGEventField,
}

const VERTICAL: ScrollAxisFields = ScrollAxisFields {
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

/// The scroll magnitude for `axis`, preferring the pixel-precise field, then the
/// fixed-point, then the integer line — the order the reference tools use, so a
/// hi-res wheel (which reports in the pixel field with the line field at 0) is
/// not mistaken for "no scroll".
#[allow(
    clippy::cast_precision_loss,
    reason = "scroll line deltas are small integers, exact in f64"
)]
fn usable_scroll_delta(event: &CGEvent, axis: ScrollAxisFields) -> f64 {
    let point = event.get_double_value_field(axis.point);
    if point != 0.0 {
        return point;
    }
    let fixed = event.get_double_value_field(axis.fixed);
    if fixed != 0.0 {
        return fixed;
    }
    event.get_integer_value_field(axis.line) as f64
}

/// Scale every delta representation attached to one scroll axis in place.
/// Applications choose independently between line, fixed-point, and point
/// deltas, so changing only the preferred representation would be inconsistent.
fn scale_scroll_axis(event: &CGEvent, axis: ScrollAxisFields, scale_percent: i16) {
    let line = event.get_integer_value_field(axis.line);
    let fixed = event.get_double_value_field(axis.fixed);
    let point = event.get_double_value_field(axis.point);
    event.set_integer_value_field(axis.line, scale_integer_delta(line, scale_percent));
    #[allow(
        clippy::cast_precision_loss,
        reason = "the percentage is a small integer represented exactly in f64"
    )]
    let precise_scale = f64::from(scale_percent) / 100.0;
    event.set_double_value_field(axis.fixed, fixed * precise_scale);
    event.set_double_value_field(axis.point, point * precise_scale);
}

/// Scale a coarse line delta and round to the nearest whole tick, with halves
/// away from zero. Saturating arithmetic keeps a malformed event fail-safe.
fn scale_integer_delta(delta: i64, scale_percent: i16) -> i64 {
    let product = delta.saturating_mul(i64::from(scale_percent));
    if product >= 0 {
        product.saturating_add(50) / 100
    } else {
        product.saturating_sub(50) / 100
    }
}

/// Create the event tap and run loop on a dedicated thread.
pub(crate) fn start(
    cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
) -> Result<HookInner, HookError> {
    if !has_accessibility() {
        return Err(HookError::AccessibilityDenied);
    }

    // Wrap in Arc so the closure handed to CGEventTap::new captures it by
    // clone rather than by move — avoids a second Box allocation.
    let cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync> = Arc::new(cb);

    let signals = Arc::new(WatchdogSignals::default());
    let lifecycle_watchdog = spawn_lifecycle_watchdog(Arc::clone(&signals))?;
    let (rl_tx, rl_rx) = mpsc::channel::<CFRunLoop>();

    let thread = {
        let thread_signals = Arc::clone(&signals);
        match thread::Builder::new()
            .name("openlogi-hook".into())
            .spawn(move || thread_main(cb, rl_tx, thread_signals))
        {
            Ok(thread) => thread,
            Err(error) => {
                signals.set_phase(TapPhase::ThreadExited);
                lifecycle_watchdog.thread().unpark();
                let _ = lifecycle_watchdog.join();
                return Err(HookError::MacOsTap(error.to_string()));
            }
        }
    };

    // Block until the background thread confirms the run loop is live, or
    // reports failure by dropping its sender.
    let Ok(run_loop) = rl_rx.recv() else {
        let error = HookError::MacOsTap(
            "background thread exited before the run loop started; \
             CGEventTapCreate likely returned null"
                .into(),
        );
        if let Err(panic) = thread.join() {
            error!(?panic, "hook thread panicked during startup");
        }
        lifecycle_watchdog.thread().unpark();
        if let Err(panic) = lifecycle_watchdog.join() {
            error!(?panic, "hook lifecycle watchdog panicked during startup");
        }
        return Err(error);
    };

    Ok(HookInner {
        thread,
        lifecycle_watchdog,
        run_loop,
        signals,
    })
}

const CALLBACK_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(20);
const LIFECYCLE_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FREEZE_HAZARD_EXIT_CODE: i32 = 78;

/// Event types the HID tap observes. Pointer *Dragged variants are required
/// because a held button makes the OS emit those instead of `MouseMoved`.
fn hooked_event_types() -> Vec<CGEventType> {
    vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        // Function-key remapper: F1–F12/Esc arrive as KeyDown/KeyUp.
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ]
}

/// Invoke the user callback under `catch_unwind`, always failing open.
fn run_tap_callback(
    cb: &dyn Fn(HookEvent) -> EventDisposition,
    etype: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // Mouse first, then keyboard; a given event type is one or the other.
        let hook_event = if let Some(mouse_event) = translate(etype, event) {
            HookEvent::Mouse(mouse_event)
        } else if let Some(key_event) = translate_key(etype, event) {
            HookEvent::Key(key_event)
        } else {
            return CallbackResult::Keep;
        };
        match cb(hook_event) {
            EventDisposition::PassThrough => CallbackResult::Keep,
            EventDisposition::Suppress => CallbackResult::Drop,
            EventDisposition::AdjustHorizontalScroll { scale_percent } => {
                if matches!(etype, CGEventType::ScrollWheel) {
                    scale_scroll_axis(event, HORIZONTAL, scale_percent);
                }
                CallbackResult::Keep
            }
        }
    }));
    if let Ok(disposition) = result {
        disposition
    } else {
        error!(
            "OS mouse-hook callback panicked — passing event through to \
             avoid wedging system input"
        );
        CallbackResult::Keep
    }
}

/// Sibling watchdog: if the callback is still entered past the budget, abort
/// the agent so macOS tears the tap down and system input recovers.
fn spawn_callback_watchdog(
    signals: Arc<WatchdogSignals>,
    in_callback: Arc<AtomicBool>,
    entered_at_ms: Arc<AtomicU64>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("openlogi-hook-watchdog".into())
        .spawn(move || {
            loop {
                let phase = signals.phase();
                if matches!(phase, TapPhase::TapStopped | TapPhase::ThreadExited) {
                    return;
                }
                thread::sleep(CALLBACK_WATCHDOG_POLL_INTERVAL);
                if !in_callback.load(Ordering::Acquire) {
                    continue;
                }
                let entered = entered_at_ms.load(Ordering::Acquire);
                if entered == 0 {
                    continue;
                }
                let Some(elapsed) = stuck_callback(signals.now_millis(), entered) else {
                    continue;
                };
                // Re-sample: a fresh high-frequency event may have rewritten
                // the stamp between the first loads and the budget check.
                if !in_callback.load(Ordering::Acquire)
                    || entered_at_ms.load(Ordering::Acquire) != entered
                {
                    continue;
                }
                if signals.phase() != TapPhase::Armed {
                    continue;
                }
                error!(
                    stuck_ms = duration_millis(elapsed),
                    "OS mouse-hook callback stuck past budget — exiting agent to \
                     restore system input (HID CGEventTap freeze hazard)"
                );
                // Hard exit: disable_tap alone cannot unblock an in-flight
                // callback, and a live active HID tap freezes all pointer I/O.
                #[expect(
                    clippy::exit,
                    reason = "this watchdog thread has no caller to return to and the stuck callback owns the active HID tap, which serialises every pointer event machine-wide; only process death makes macOS tear the tap down"
                )]
                std::process::exit(FREEZE_HAZARD_EXIT_CODE);
            }
        })
        .map(|_| ())
}

/// Independent lifecycle watchdog for paths that never enter the Rust tap
/// callback (for example, TCC revocation wedging `CFRunLoopRunInMode` or the
/// Accessibility query itself).
///
/// This thread is started before the tap thread, uses only atomics and a
/// monotonic clock, and remains armed after a stop request. It deliberately
/// does not call `has_accessibility()`: that query can itself stop returning
/// after TCC revocation. It disarms only after the tap thread reports that the
/// tap was synchronously disabled and destroyed, or (for explicit shutdown)
/// after that thread has exited.
fn spawn_lifecycle_watchdog(
    signals: Arc<WatchdogSignals>,
) -> Result<thread::JoinHandle<()>, HookError> {
    thread::Builder::new()
        .name("openlogi-hook-lifecycle-watchdog".into())
        .spawn(move || {
            let mut watchdog = LifecycleWatchdog::default();
            loop {
                let observation = LifecycleObservation {
                    phase: signals.phase(),
                    stop_requested: signals.stop_requested(),
                    tap_progress_at: signals.tap_progress_at(),
                };
                match watchdog.evaluate(signals.now(), observation) {
                    LifecycleDecision::Continue => {
                        thread::park_timeout(LIFECYCLE_WATCHDOG_POLL_INTERVAL);
                    }
                    LifecycleDecision::Complete => return,
                    LifecycleDecision::Exit { reason, elapsed } => {
                        // The tap thread may have completed immediately after
                        // the decision. Only a still-hazardous phase may exit.
                        let phase = signals.phase();
                        let still_hazardous = match reason {
                            LifecycleExitReason::TapThreadStalled => {
                                matches!(phase, TapPhase::Arming | TapPhase::Armed)
                            }
                            LifecycleExitReason::StopTimedOut => phase != TapPhase::ThreadExited,
                        };
                        if !still_hazardous {
                            continue;
                        }
                        let reason = match reason {
                            LifecycleExitReason::TapThreadStalled if phase == TapPhase::Arming => {
                                "HID tap creation or activation stopped making progress"
                            }
                            LifecycleExitReason::TapThreadStalled => {
                                "HID tap thread stopped making progress while tap remained active"
                            }
                            LifecycleExitReason::StopTimedOut => {
                                "hook stop requested but tap thread did not exit"
                            }
                        };
                        error!(
                            reason,
                            elapsed_ms = duration_millis(elapsed),
                            ?phase,
                            "HID CGEventTap lifecycle did not make progress before deadline — \
                             exiting agent to restore system input"
                        );
                        #[expect(
                            clippy::exit,
                            reason = "the tap thread is wedged (TCC revocation can stall it inside CoreGraphics), so no unwinding path can reach it from this watchdog thread; a live HID tap left behind freezes all input until the process dies"
                        )]
                        std::process::exit(FREEZE_HAZARD_EXIT_CODE);
                    }
                }
            }
        })
        .map_err(|error| HookError::MacOsTap(format!("could not spawn tap watchdog: {error}")))
}

/// Body of the background hook thread.
#[allow(
    clippy::needless_pass_by_value,
    reason = "rl_tx must be owned: dropping it signals the parent's recv() to return Err on failure paths"
)]
fn thread_main(
    cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync>,
    rl_tx: mpsc::Sender<CFRunLoop>,
    signals: Arc<WatchdogSignals>,
) {
    // Declared first so it drops last, after the tap, callback, source, and run
    // loop locals have unwound. The lifecycle watchdog treats this notification
    // as proof that an explicit stop has completed, not merely been requested.
    let _thread_exit = signals.thread_exit_guard();

    // A successful CGEventTapCreate may install the HID tap before returning,
    // so lifecycle monitoring must be armed before entering CoreGraphics.
    signals.mark_tap_progress();
    signals.set_phase(TapPhase::Arming);

    let in_callback = Arc::new(AtomicBool::new(false));
    let entered_at_ms = Arc::new(AtomicU64::new(0));

    let tap_result = {
        let callback_signals = Arc::clone(&signals);
        let in_callback = Arc::clone(&in_callback);
        let entered_at_ms = Arc::clone(&entered_at_ms);
        CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            hooked_event_types(),
            move |_proxy: CGEventTapProxy, etype: CGEventType, event: &CGEvent| {
                entered_at_ms.store(callback_signals.now_millis(), Ordering::Relaxed);
                in_callback.store(true, Ordering::Release);
                let disposition = run_tap_callback(cb.as_ref(), etype, event);
                in_callback.store(false, Ordering::Release);
                disposition
            },
        )
    };

    let Ok(tap) = tap_result else {
        error!("CGEventTapCreate returned null — Accessibility may have been revoked");
        // Dropping rl_tx causes rl_rx.recv() on the parent to return Err,
        // which we surface as MacOsTap.
        return;
    };
    signals.mark_tap_progress();

    let Ok(loop_source) = tap.mach_port().create_runloop_source(0) else {
        error!("CFRunLoopSourceCreate failed for event tap");
        return;
    };
    signals.mark_tap_progress();

    let run_loop = CFRunLoop::get_current();

    // SAFETY: kCFRunLoopCommonModes is a static CF string constant that
    // lives for the duration of the process.
    unsafe {
        run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }
    signals.mark_tap_progress();
    if let Err(error) = spawn_callback_watchdog(
        Arc::clone(&signals),
        Arc::clone(&in_callback),
        Arc::clone(&entered_at_ms),
    ) {
        error!(%error, "could not spawn callback watchdog — refusing to arm HID tap");
        return;
    }
    signals.mark_tap_progress();
    tap.enable();
    signals.mark_tap_progress();
    signals.set_phase(TapPhase::Armed);

    if rl_tx.send(run_loop.clone()).is_err() {
        debug!("hook parent dropped before run loop was ready; stopping");
        disable_tap(&tap);
        // SAFETY: framework-provided static CFStringRef, 'static.
        run_loop.remove_source(&loop_source, unsafe { kCFRunLoopCommonModes });
        drop(loop_source);
        drop(tap);
        signals.set_phase(TapPhase::TapStopped);
        return;
    }

    // Service the tap in short slices instead of an unbounded
    // `run_current()`. Between slices we re-check Accessibility: an active
    // tap at the HID location that outlives its permission wedges the
    // *entire* system input stream — mouse and keyboard alike — until
    // reboot. If the user revokes access while we're live, tear the tap
    // down right here, on the tap's own thread, so input is restored even
    // when the UI thread is already stuck.
    //
    // `stop()` requests shutdown two ways: it sets the stop latch and calls
    // `run_loop.stop()`. The CF stop returns `Stopped` and breaks promptly
    // while a slice is running, but is a no-op if it lands in the gap
    // between slices (CFRunLoopStop only acts on a running loop). The latch,
    // checked at the top of every slice, is the reliable signal: in
    // that race the thread notices one 500 ms slice later instead of joining
    // forever.
    loop {
        if signals.stop_requested() {
            break;
        }
        signals.mark_tap_progress();
        match CFRunLoop::run_in_mode(
            // SAFETY: framework-provided static CFStringRef, 'static.
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(500),
            false,
        ) {
            CFRunLoopRunResult::Stopped | CFRunLoopRunResult::Finished => break,
            CFRunLoopRunResult::TimedOut | CFRunLoopRunResult::HandledSource => {}
        }
        signals.mark_tap_progress();
        if !has_accessibility() {
            warn!(
                "Accessibility revoked while the event tap was live — \
                 disabling the tap to avoid wedging system input"
            );
            break;
        }
        // Recover from an OS-initiated disable (TapDisabledByTimeout/UserInput):
        // re-enabling is idempotent while the tap is already live, so this brings
        // a disabled tap back within one slice instead of the hook going
        // permanently deaf. Only reached while Accessibility is still granted.
        tap.enable();
    }

    // Detach the tap from the event stream synchronously before unwinding,
    // so input recovers immediately rather than whenever CF happens to
    // release the port.
    disable_tap(&tap);
    // SAFETY: framework-provided static CFStringRef, 'static.
    run_loop.remove_source(&loop_source, unsafe { kCFRunLoopCommonModes });
    drop(loop_source);
    drop(tap);
    signals.set_phase(TapPhase::TapStopped);
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Disable an active event tap now. core-graphics only exposes the enable
/// side of `CGEventTapEnable`, so we bind the disable side ourselves.
fn disable_tap(tap: &CGEventTap) {
    use core_foundation::base::TCFType as _;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapEnable(tap: core_foundation::mach_port::CFMachPortRef, enable: bool);
    }

    // SAFETY: `tap.mach_port()` is a live `CFMachPort` for the duration of
    // the call; `CGEventTapEnable(.., false)` is idempotent and merely
    // detaches the tap from the system event stream.
    unsafe { CGEventTapEnable(tap.mach_port().as_concrete_TypeRef(), false) };
}

/// Mirror of CoreGraphics' `CGEventTapInformation`. `#[repr(C)]` reproduces the
/// header layout (including the padding before `events_of_interest` and
/// `min_usec_latency`) so `CGGetEventTapList` writes into the right offsets.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "events_of_interest and the latency floats are unread but must \
              exist so the struct keeps CoreGraphics' exact 48-byte stride; \
              CGGetEventTapList writes whole records into the buffer"
)]
struct CGEventTapInformation {
    event_tap_id: u32,
    tap_point: u32,
    options: u32,
    events_of_interest: u64,
    tapping_process: i32,
    process_being_tapped: i32,
    enabled: bool,
    min_usec_latency: f32,
    avg_usec_latency: f32,
    max_usec_latency: f32,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // `core-graphics` doesn't bind the enumeration side (it ships the tap
    // *create/enable* path only), so we declare it ourselves. Passing a null
    // list with count 0 returns the number of taps via `event_tap_count`.
    fn CGGetEventTapList(
        max_number_of_taps: u32,
        tap_list: *mut CGEventTapInformation,
        event_tap_count: *mut u32,
    ) -> i32;
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    // libproc; resolves a PID to its executable path. Returns the byte length
    // written, or <= 0 on failure (e.g. the process exited, or it's out of the
    // caller's permission scope).
    fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffersize: u32) -> i32;
}

/// See [`super::Hook::list_event_taps`].
pub(crate) fn list_event_taps() -> Vec<EventTapInfo> {
    let mut count: u32 = 0;
    // SAFETY: a null `tap_list` with `max == 0` is the documented count-probe
    // form; it only writes `count`.
    let err = unsafe { CGGetEventTapList(0, std::ptr::null_mut(), &raw mut count) };
    if err != 0 || count == 0 {
        return Vec::new();
    }

    // SAFETY: `CGEventTapInformation` is a plain `repr(C)` POD; an all-zero bit
    // pattern is a valid instance (`enabled = false`, all numeric fields 0).
    // `CGGetEventTapList` overwrites each slot it fills.
    let mut taps: Vec<CGEventTapInformation> = vec![unsafe { std::mem::zeroed() }; count as usize];
    // SAFETY: `taps` holds exactly `count` initialised, correctly aligned slots
    // and stays alive for the call, and that same `count` is the maximum passed
    // in, so the C side cannot write past the allocation; the out-parameter
    // points at a live local it may only overwrite.
    let err = unsafe { CGGetEventTapList(count, taps.as_mut_ptr(), &raw mut count) };
    if err != 0 {
        return Vec::new();
    }
    // The second call may report fewer taps than the probe; never read past it.
    taps.truncate(count as usize);

    taps.into_iter()
        .map(|t| EventTapInfo {
            tap_id: t.event_tap_id,
            location: match t.tap_point {
                0 => TapLocation::Hid,
                1 => TapLocation::Session,
                2 => TapLocation::AnnotatedSession,
                other => TapLocation::Other(other),
            },
            // kCGEventTapOptionDefault == 0 (active); kCGEventTapOptionListenOnly == 1.
            active: t.options == 0,
            enabled: t.enabled,
            owner_pid: t.tapping_process,
            owner_name: process_name(t.tapping_process),
            target_pid: (t.process_being_tapped != 0).then_some(t.process_being_tapped),
        })
        .collect()
}

/// Best-effort PID → executable file name via libproc.
fn process_name(pid: i32) -> Option<String> {
    // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN (4 * 1024).
    const BUF_LEN: u32 = 4096;
    if pid <= 0 {
        return None;
    }
    let mut buf = vec![0u8; BUF_LEN as usize];
    // SAFETY: `buf` is a live, writable buffer of `BUF_LEN` bytes; the C side
    // writes at most that many and returns the length actually written.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), BUF_LEN) };
    if len <= 0 {
        return None;
    }
    // `len > 0` here, so `unsigned_abs` is the value itself; widening to usize
    // is lossless and sidesteps the sign-loss cast lint.
    buf.truncate(len.unsigned_abs() as usize);
    let path = String::from_utf8_lossy(&buf);
    Some(path.rsplit('/').next().unwrap_or(&path).to_string())
}

/// Signal the run loop to stop and join the background thread.
pub(crate) fn stop(inner: HookInner) {
    // Latch stop before waking either thread. The lifecycle watchdog stays
    // armed across the blocking join and accepts only `ThreadExited` as proof
    // that explicit shutdown completed.
    inner.signals.request_stop();
    inner.lifecycle_watchdog.thread().unpark();
    inner.run_loop.stop();
    if let Err(e) = inner.thread.join() {
        error!("hook thread panicked on shutdown: {e:?}");
    }
    inner.lifecycle_watchdog.thread().unpark();
    if let Err(e) = inner.lifecycle_watchdog.join() {
        error!("hook lifecycle watchdog panicked on shutdown: {e:?}");
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::*;

    #[test]
    fn tap_callback_suppresses_normally_and_passes_through_panics() {
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .expect("CGEventSourceCreate must succeed");
        let event = CGEvent::new(source).expect("CGEventCreate must succeed");

        assert!(matches!(
            run_tap_callback(
                &|_| EventDisposition::Suppress,
                CGEventType::MouseMoved,
                &event
            ),
            CallbackResult::Drop
        ));
        assert!(matches!(
            run_tap_callback(
                &|_| panic!("test callback panic"),
                CGEventType::MouseMoved,
                &event
            ),
            CallbackResult::Keep
        ));
    }

    #[test]
    fn scroll_axis_scale_updates_line_fixed_and_point_deltas() {
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .expect("CGEventSourceCreate must succeed");
        let event = CGEvent::new(source).expect("CGEventCreate must succeed");
        event.set_type(CGEventType::ScrollWheel);
        event.set_integer_value_field(HORIZONTAL.line, -2);
        event.set_double_value_field(HORIZONTAL.fixed, -1.25);
        event.set_double_value_field(HORIZONTAL.point, -4.0);

        scale_scroll_axis(&event, HORIZONTAL, -300);

        assert_eq!(event.get_integer_value_field(HORIZONTAL.line), 6);
        let fixed = event.get_double_value_field(HORIZONTAL.fixed);
        assert!((fixed - 3.75).abs() < f64::EPSILON, "fixed delta: {fixed}");
        let point = event.get_double_value_field(HORIZONTAL.point);
        assert!((point - 12.0).abs() < f64::EPSILON, "point delta: {point}");
    }

    #[test]
    fn tap_callback_adjusts_only_horizontal_scroll_fields() {
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .expect("CGEventSourceCreate must succeed");
        let event = CGEvent::new(source).expect("CGEventCreate must succeed");
        event.set_type(CGEventType::ScrollWheel);
        event.set_integer_value_field(VERTICAL.line, 2);
        event.set_integer_value_field(HORIZONTAL.line, -1);
        event.set_integer_value_field(SCROLL_PHASE, 1);
        event.set_integer_value_field(SCROLL_COUNT, 7);
        event.set_integer_value_field(MOMENTUM_PHASE, 2);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, 42);

        let result = run_tap_callback(
            &|_| EventDisposition::AdjustHorizontalScroll {
                scale_percent: -300,
            },
            CGEventType::ScrollWheel,
            &event,
        );

        assert!(matches!(result, CallbackResult::Keep));
        assert_eq!(event.get_integer_value_field(VERTICAL.line), 2);
        assert_eq!(event.get_integer_value_field(HORIZONTAL.line), 3);
        assert_eq!(event.get_integer_value_field(SCROLL_PHASE), 1);
        assert_eq!(event.get_integer_value_field(SCROLL_COUNT), 7);
        assert_eq!(event.get_integer_value_field(MOMENTUM_PHASE), 2);
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            42
        );
    }

    #[test]
    fn integer_scroll_scaling_rounds_halves_away_from_zero() {
        assert_eq!(scale_integer_delta(1, 150), 2);
        assert_eq!(scale_integer_delta(-1, 150), -2);
        assert_eq!(scale_integer_delta(1, -150), -2);
    }
}

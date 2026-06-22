//! macOS `CGEventTap` implementation of the OS-level mouse hook.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread;

use core_foundation::base::{CFTypeRef, TCFType as _};
use core_foundation::number::CFNumber;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEvent, CGEventField, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult, EventField,
};
use foreign_types_shared::ForeignType as _;
use tracing::{debug, error, warn};

use crate::{ButtonId, EventDevice, EventDisposition, HookError, MouseEvent, ScrollTransform};

/// Everything `Hook` needs to control the background thread.
pub(crate) struct HookInner {
    thread: thread::JoinHandle<()>,
    run_loop: CFRunLoop,
}

// SAFETY: CFRunLoop is a Core Foundation ref-counted object. The CF
// documentation states that CFRunLoop objects can be passed between
// threads; only CFRunLoopRun must be called on the owning thread.
unsafe impl Send for HookInner {}

// Raw FFI for `AXIsProcessTrustedWithOptions` from the Accessibility
// framework. Passing `NULL` queries trust state without prompting; passing
// a dictionary with `kAXTrustedCheckOptionPrompt = true` raises the system
// permission dialog and registers the process in the Accessibility list.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
}

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
                    // SAFETY: a numeric property is a +1 CFNumber; wrap takes ownership.
                    service_property(service, k)
                        .and_then(|p| {
                            unsafe { CFNumber::wrap_under_create_rule(p.cast()) }.to_i64()
                        })
                        .and_then(|n| u32::try_from(n).ok())
                };
                // SAFETY: a String property is a +1 CFString; wrap takes ownership.
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
    // SAFETY: NULL is documented as a valid argument; it queries the current
    // trust state without raising a permission dialog.
    unsafe { AXIsProcessTrustedWithOptions(std::ptr::null()) }
}

/// Raise the Accessibility prompt + register the process. See
/// [`super::Hook::prompt_accessibility`].
pub(crate) fn prompt_accessibility() {
    use core_foundation::base::TCFType as _;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    // SAFETY: `kAXTrustedCheckOptionPrompt` is a framework-provided
    // `CFStringRef` constant; wrapping under the get rule borrows it
    // without taking ownership.
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options =
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
    // SAFETY: `options` is a valid `CFDictionaryRef` for the lifetime of
    // the call; the function reads it and (if untrusted) shows the dialog.
    // The returned trust state is observed separately via the watcher.
    let _trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast()) };
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

fn transform_scroll_event(event: &CGEvent, transform: ScrollTransform) {
    let factor = if transform.inverted { -1.0 } else { 1.0 } * f64::from(transform.strength.max(1));
    let tactility = i64::from(transform.tactility.min(10));

    transform_double_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
        factor,
        tactility,
    );
    transform_double_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
        factor,
        tactility,
    );
    transform_integer_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
        factor,
        0,
    );
    transform_integer_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
        factor,
        0,
    );
    transform_integer_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
        factor,
        0,
    );
    transform_integer_scroll_field(
        event,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
        factor,
        0,
    );
}

fn transform_double_scroll_field(
    event: &CGEvent,
    field: core_graphics::event::CGEventField,
    factor: f64,
    tactility: i64,
) {
    let value = event.get_double_value_field(field) * factor;
    #[allow(
        clippy::cast_precision_loss,
        reason = "scroll deltas are small event-local values; quantization outputs fit safely in f64 precision"
    )]
    let value = if tactility <= 1 {
        value
    } else {
        quantize_scroll(value, tactility) as f64
    };
    event.set_double_value_field(field, value);
}

fn transform_integer_scroll_field(
    event: &CGEvent,
    field: core_graphics::event::CGEventField,
    factor: f64,
    tactility: i64,
) {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "CGEvent scroll payloads are small bounded deltas; we intentionally round the scaled value back to i64 fields"
    )]
    let value = (event.get_integer_value_field(field) as f64 * factor).round() as i64;
    #[allow(
        clippy::cast_precision_loss,
        reason = "the rounded i64 is an event-local wheel delta before quantization, not an unbounded accumulator"
    )]
    let value = if tactility <= 1 {
        value
    } else {
        quantize_scroll(value as f64, tactility)
    };
    event.set_integer_value_field(field, value);
}

fn quantize_scroll(value: f64, tactility: i64) -> i64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "quantization intentionally snaps the event-local floating delta to an integer wheel step"
    )]
    let v = value.round() as i64;
    if tactility <= 1 {
        return v;
    }

    let abs = v.abs();
    let snapped = ((abs + tactility / 2) / tactility) * tactility;
    if snapped == 0 && v != 0 {
        return v.signum() * tactility;
    }
    if v < 0 { -snapped } else { snapped }
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
        }),
        CGEventType::LeftMouseUp => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: false,
        }),
        CGEventType::RightMouseDown => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: true,
        }),
        CGEventType::RightMouseUp => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: false,
        }),
        CGEventType::OtherMouseDown => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button { id, pressed: true })
        }
        CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button { id, pressed: false })
        }
        CGEventType::ScrollWheel => {
            // axis 1 = vertical scroll; axis 2 = horizontal scroll. Read the
            // pixel-precise delta in preference to the coarse line delta (a hi-res
            // wheel reports its motion in the pixel field with the line field at 0,
            // so reading only the line field would look like "no scroll").
            let dy = usable_scroll_delta(event, VERTICAL);
            let dx = usable_scroll_delta(event, HORIZONTAL);
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

/// Create the event tap and run loop on a dedicated thread.
pub(crate) fn start(
    cb: impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static,
) -> Result<HookInner, HookError> {
    if !has_accessibility() {
        return Err(HookError::AccessibilityDenied);
    }

    // Wrap in Arc so the closure handed to CGEventTap::new captures it by
    // clone rather than by move — avoids a second Box allocation.
    let cb: Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync> = Arc::new(cb);

    let (rl_tx, rl_rx) = mpsc::channel::<CFRunLoop>();

    let thread = thread::Builder::new()
        .name("openlogi-hook".into())
        .spawn(move || thread_main(cb, rl_tx))
        .map_err(|e| HookError::MacOsTap(e.to_string()))?;

    // Block until the background thread confirms the run loop is live, or
    // reports failure by dropping its sender.
    let run_loop = rl_rx.recv().map_err(|_| {
        HookError::MacOsTap(
            "background thread exited before the run loop started; \
             CGEventTapCreate likely returned null"
                .into(),
        )
    })?;

    Ok(HookInner { thread, run_loop })
}

/// Body of the background hook thread.
#[allow(
    clippy::needless_pass_by_value,
    reason = "rl_tx must be owned: dropping it signals the parent's recv() to return Err on failure paths"
)]
fn thread_main(
    cb: Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync>,
    rl_tx: mpsc::Sender<CFRunLoop>,
) {
    let event_types = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
        // Pointer movement, for gesture-button hold+swipe. A held button makes
        // the OS emit *Dragged rather than MouseMoved, so all four are needed.
        // The callback stays lock-light (see `hook_runtime`) so this high-rate
        // stream can't stall the tap.
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
    ];

    let tap_result = CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        event_types,
        move |_proxy: CGEventTapProxy, etype: CGEventType, event: &CGEvent| {
            let Some(mouse_event) = translate(etype, event) else {
                return CallbackResult::Keep;
            };
            match cb(mouse_event) {
                EventDisposition::PassThrough => CallbackResult::Keep,
                EventDisposition::Suppress => CallbackResult::Drop,
                EventDisposition::TransformScroll(transform) => {
                    transform_scroll_event(event, transform);
                    CallbackResult::Keep
                }
            }
        },
    );

    let Ok(tap) = tap_result else {
        error!("CGEventTapCreate returned null — Accessibility may have been revoked");
        // Dropping rl_tx causes rl_rx.recv() on the parent to return Err,
        // which we surface as MacOsTap.
        return;
    };

    let Ok(loop_source) = tap.mach_port().create_runloop_source(0) else {
        error!("CFRunLoopSourceCreate failed for event tap");
        return;
    };

    let run_loop = CFRunLoop::get_current();

    // SAFETY: kCFRunLoopCommonModes is a static CF string constant that
    // lives for the duration of the process.
    unsafe {
        run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }
    tap.enable();

    if rl_tx.send(run_loop).is_err() {
        debug!("hook parent dropped before run loop was ready; stopping");
        return;
    }

    // Service the tap in short slices instead of an unbounded
    // `run_current()`. Between slices we re-check Accessibility: an active
    // tap at the HID location that outlives its permission wedges the
    // *entire* system input stream — mouse and keyboard alike — until
    // reboot. If the user revokes access while we're live, tear the tap
    // down right here, on the tap's own thread, so input is restored even
    // when the UI thread is already stuck. `stop()` (normal shutdown)
    // returns `Stopped` and also breaks the loop.
    loop {
        match CFRunLoop::run_in_mode(
            // SAFETY: framework-provided static CFStringRef, 'static.
            unsafe { kCFRunLoopDefaultMode },
            std::time::Duration::from_millis(500),
            false,
        ) {
            CFRunLoopRunResult::Stopped | CFRunLoopRunResult::Finished => break,
            CFRunLoopRunResult::TimedOut | CFRunLoopRunResult::HandledSource => {}
        }
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

/// Signal the run loop to stop and join the background thread.
pub(crate) fn stop(inner: HookInner) {
    inner.run_loop.stop();
    if let Err(e) = inner.thread.join() {
        error!("hook thread panicked on shutdown: {e:?}");
    }
}

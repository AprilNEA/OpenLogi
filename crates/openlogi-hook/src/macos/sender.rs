//! Which device produced a `CGEvent`: the IOHID sender-id lookup behind the
//! event, and the IOKit registry walk that turns that id into device facts.
//!
//! Both rest on symbols no crate binds, so their `extern` declarations live
//! here, next to their only callers.

use std::cell::RefCell;
use std::collections::HashMap;

use core_foundation::base::{CFTypeRef, TCFType as _};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::CGEvent;
use foreign_types_shared::ForeignType as _;

use crate::EventDevice;

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
pub(super) fn event_sender_id(event: &CGEvent) -> Option<u64> {
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
pub(super) struct SenderDeviceInfo {
    pub(super) event_device: EventDevice,
    pub(super) is_trackpad: bool,
}

/// Device facts for the registry id `sender_id`. A trackpad presents a *mouse*
/// HID interface for scrolling, so usage can't separate it from a real wheel;
/// product identity stays stable across wheel modes, unlike CGEvent phase.
/// Cached per id because the registry walk is slow and identity never changes.
pub(super) fn sender_device_info(sender_id: u64) -> SenderDeviceInfo {
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

//! Physical-device attribution for macOS button events whose HID sender is zero.

use std::collections::HashMap;
use std::ffi::c_void;

use core_foundation::base::{CFTypeRef, TCFType as _};
use core_foundation::string::{CFString, CFStringRef};
use tracing::warn;

use crate::EventDevice;

type IOHIDManagerRef = *mut c_void;
type IOHIDDeviceRef = *mut c_void;
type IOHIDElementRef = *mut c_void;
type IOHIDValueRef = *mut c_void;
type CFSetRef = *const c_void;
type CFArrayRef = *const c_void;

const IO_RETURN_SUCCESS: i32 = 0;
const HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const HID_USAGE_MOUSE: u32 = 0x02;
const HID_PAGE_BUTTON: u32 = 0x09;
const CF_NUMBER_SINT64_TYPE: i32 = 4;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFTypeRef, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: CFTypeRef);
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> i32;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> i32;
    fn IOHIDManagerCopyDevices(manager: IOHIDManagerRef) -> CFSetRef;
    fn IOHIDDeviceGetProperty(device: IOHIDDeviceRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDDeviceCopyMatchingElements(
        device: IOHIDDeviceRef,
        matching: CFTypeRef,
        options: u32,
    ) -> CFArrayRef;
    fn IOHIDDeviceGetValue(
        device: IOHIDDeviceRef,
        element: IOHIDElementRef,
        value: *mut IOHIDValueRef,
    ) -> i32;
    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFSetGetCount(set: CFSetRef) -> isize;
    fn CFSetGetValues(set: CFSetRef, values: *mut *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFTypeRef, number_type: i32, value: *mut c_void) -> bool;
    fn CFStringGetTypeID() -> usize;
}

/// Long-lived manager used only for rare sender-less button transitions.
struct HidManager(IOHIDManagerRef);

// SAFETY: IOHIDManager is a Core Foundation object that may be transferred
// between threads. Ownership is exclusive and every operation is serialized by
// the event-tap callback's RefCell, so it is never accessed concurrently.
unsafe impl Send for HidManager {}

impl HidManager {
    fn open() -> Result<Self, i32> {
        // SAFETY: a null allocator selects the process default; zero options are documented.
        let manager = unsafe { IOHIDManagerCreate(std::ptr::null(), 0) };
        if manager.is_null() {
            return Err(-1);
        }
        // SAFETY: `manager` is live; null matching means all HID devices.
        unsafe { IOHIDManagerSetDeviceMatching(manager, std::ptr::null()) };
        // SAFETY: `manager` is live and opened once with documented zero options.
        let result = unsafe { IOHIDManagerOpen(manager, 0) };
        if result == IO_RETURN_SUCCESS {
            return Ok(Self(manager));
        }
        // SAFETY: creation returned a +1 object which must be released on failure.
        unsafe { CFRelease(manager.cast_const()) };
        Err(result)
    }

    fn pressed_devices(&self, button_number: i64) -> Vec<ButtonCandidate> {
        let Ok(usage) = u32::try_from(button_number + 1) else {
            return Vec::new();
        };
        // SAFETY: the manager stays open for `self`; Copy returns a +1 set or null.
        let devices = unsafe { IOHIDManagerCopyDevices(self.0) };
        if devices.is_null() {
            return Vec::new();
        }
        let candidates = device_values(devices)
            .into_iter()
            .filter_map(|device| candidate_for_button(device.cast_mut(), usage))
            .collect();
        // SAFETY: balance the +1 returned by IOHIDManagerCopyDevices.
        unsafe { CFRelease(devices) };
        candidates
    }
}

impl Drop for HidManager {
    fn drop(&mut self) {
        // SAFETY: this is the only owner and the manager is still live.
        unsafe {
            let _ = IOHIDManagerClose(self.0, 0);
            CFRelease(self.0.cast_const());
        }
    }
}

#[derive(Clone, Debug)]
struct ButtonCandidate {
    device: EventDevice,
    pressed: bool,
}

/// Resolves and caches device identity across a sender-less down/up pair.
pub(super) struct SenderlessButtonResolver {
    manager: Option<HidManager>,
    held_sources: HashMap<i64, EventDevice>,
}

impl SenderlessButtonResolver {
    pub(super) fn new() -> Self {
        let manager = match HidManager::open() {
            Ok(manager) => Some(manager),
            Err(code) => {
                warn!(
                    code = format_args!("{code:#x}"),
                    "could not open IOHIDManager for sender-less button attribution"
                );
                None
            }
        };
        Self {
            manager,
            held_sources: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn unavailable() -> Self {
        Self {
            manager: None,
            held_sources: HashMap::new(),
        }
    }

    /// Drop every cached attribution. The OS tap being disabled and
    /// re-enabled (`TapDisabledByTimeout`/`TapDisabledByUserInput`) can drop
    /// button-up events without this resolver ever seeing them, so a cached
    /// source would otherwise outlive the physical hold it was attributed to
    /// and later mis-attribute an unrelated button's release.
    pub(super) fn cancel_all(&mut self) {
        self.held_sources.clear();
    }

    pub(super) fn resolve(
        &mut self,
        button_number: i64,
        pressed: bool,
        sender_source: Option<EventDevice>,
    ) -> Option<EventDevice> {
        if let Some(source) = sender_source {
            if !pressed {
                self.held_sources.remove(&button_number);
            }
            return Some(source);
        }
        if !pressed {
            return self.held_sources.remove(&button_number);
        }
        let candidates = self.manager.as_ref()?.pressed_devices(button_number);
        self.resolve_press(button_number, &candidates)
    }

    /// The new-press half of [`Self::resolve`], taking already-read
    /// candidates directly so it can be exercised without a live
    /// `IOHIDManager` — see the tests module.
    fn resolve_press(
        &mut self,
        button_number: i64,
        candidates: &[ButtonCandidate],
    ) -> Option<EventDevice> {
        let Some(source) = unique_pressed_logitech(candidates) else {
            // An ambiguous new press (e.g. a second mouse pressing the same
            // button number while a prior press is still held) invalidates
            // any stale cache entry for this button: nothing proves a later
            // release belongs to the device that earned the cached
            // attribution rather than to this new, unattributable press.
            // Without this, that release would wrongly end the cached
            // device's hold instead of passing through unattributed like its
            // own down did.
            self.held_sources.remove(&button_number);
            return None;
        };
        self.held_sources.insert(button_number, source.clone());
        Some(source)
    }
}

fn device_values(devices: CFSetRef) -> Vec<*const c_void> {
    // SAFETY: `devices` is a live CFSet during this call.
    let count = unsafe { CFSetGetCount(devices) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut values = vec![std::ptr::null(); count];
    // SAFETY: `values` contains exactly `count` writable pointer slots.
    unsafe { CFSetGetValues(devices, values.as_mut_ptr()) };
    values
}

fn candidate_for_button(device: IOHIDDeviceRef, usage: u32) -> Option<ButtonCandidate> {
    if device_number(device, "PrimaryUsagePage")? != u64::from(HID_PAGE_GENERIC_DESKTOP)
        || device_number(device, "PrimaryUsage")? != u64::from(HID_USAGE_MOUSE)
    {
        return None;
    }
    let pressed = button_value(device, usage)? != 0;
    Some(ButtonCandidate {
        device: EventDevice {
            vendor_id: property_u32(device, "VendorID"),
            product_id: property_u32(device, "ProductID"),
            product_name: device_string(device, "Product"),
        },
        pressed,
    })
}

fn button_value(device: IOHIDDeviceRef, usage: u32) -> Option<isize> {
    // SAFETY: `device` is retained by the copied device set; null matches all elements.
    let elements = unsafe { IOHIDDeviceCopyMatchingElements(device, std::ptr::null(), 0) };
    if elements.is_null() {
        return None;
    }
    let value = find_button_value(device, elements, usage);
    // SAFETY: balance the +1 returned by IOHIDDeviceCopyMatchingElements.
    unsafe { CFRelease(elements) };
    value
}

fn find_button_value(device: IOHIDDeviceRef, elements: CFArrayRef, usage: u32) -> Option<isize> {
    // SAFETY: `elements` is a live CFArray during this call.
    let count = unsafe { CFArrayGetCount(elements) };
    for index in 0..count {
        // SAFETY: `index` is within the array count; the array retains the element.
        let element = unsafe { CFArrayGetValueAtIndex(elements, index) }.cast_mut();
        // SAFETY: `element` came from the device's element array and is live.
        let matches = unsafe {
            IOHIDElementGetUsagePage(element) == HID_PAGE_BUTTON
                && IOHIDElementGetUsage(element) == usage
        };
        if matches {
            return current_value(device, element);
        }
    }
    None
}

fn current_value(device: IOHIDDeviceRef, element: IOHIDElementRef) -> Option<isize> {
    let mut value = std::ptr::null_mut();
    // SAFETY: device and element belong to the live copied device set/element array.
    let result = unsafe { IOHIDDeviceGetValue(device, element, &raw mut value) };
    if result != IO_RETURN_SUCCESS || value.is_null() {
        return None;
    }
    // SAFETY: a successful read returned a live value borrowed from IOHIDDevice.
    Some(unsafe { IOHIDValueGetIntegerValue(value) })
}

fn property_u32(device: IOHIDDeviceRef, key: &str) -> Option<u32> {
    device_number(device, key).and_then(|value| u32::try_from(value).ok())
}

/// Read a `CFString`-typed HID device property, e.g. `"Product"` — the same
/// key [`crate::EventDevice::is_trackpad_like`] matches on. Without this, a
/// candidate built here always carries `product_name: None`, so a Logitech
/// touchpad exposing a mouse HID interface would pass the trackpad check by
/// omission and become remappable through `is_logitech()` alone.
fn device_string(device: IOHIDDeviceRef, key: &str) -> Option<String> {
    let key = CFString::new(key);
    // SAFETY: `device` is live and `key` is a valid CFString for this call.
    let property = unsafe { IOHIDDeviceGetProperty(device, key.as_concrete_TypeRef()) };
    // SAFETY: Core Foundation type-id queries accept any non-null CF object.
    if property.is_null() || unsafe { CFGetTypeID(property) != CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: `IOHIDDeviceGetProperty` follows the "get" rule (no retain
    // transferred to the caller) and the type-id check above proves
    // `property` is a CFString; `wrap_under_get_rule` borrows it just long
    // enough to copy the text out, matching `device_number`'s treatment of
    // the same API's CFNumber results.
    Some(unsafe { CFString::wrap_under_get_rule(property.cast()) }.to_string())
}

fn device_number(device: IOHIDDeviceRef, key: &str) -> Option<u64> {
    let key = CFString::new(key);
    // SAFETY: `device` is live and `key` is a valid CFString for this call.
    let property = unsafe { IOHIDDeviceGetProperty(device, key.as_concrete_TypeRef()) };
    // SAFETY: Core Foundation type-id queries accept any non-null CF object.
    if property.is_null() || unsafe { CFGetTypeID(property) != CFNumberGetTypeID() } {
        return None;
    }
    let mut value = 0_i64;
    // SAFETY: type-id validation above proves `property` is a CFNumber; output is i64.
    let read = unsafe {
        CFNumberGetValue(
            property,
            CF_NUMBER_SINT64_TYPE,
            (&raw mut value).cast::<c_void>(),
        )
    };
    (read && value >= 0).then_some(value.unsigned_abs())
}

fn unique_pressed_logitech(candidates: &[ButtonCandidate]) -> Option<EventDevice> {
    let mut pressed = candidates.iter().filter(|candidate| candidate.pressed);
    let source = pressed.next()?;
    if pressed.next().is_some() || !source.device.is_logitech() {
        return None;
    }
    Some(source.device.clone())
}

#[cfg(test)]
mod tests;

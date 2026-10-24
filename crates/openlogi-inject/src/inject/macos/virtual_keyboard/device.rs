//! The `IOHIDUserDevice` behind the virtual keyboard.
//!
//! The device is never scheduled on a dispatch queue, so it needs no
//! activation and no cancel handler: releasing the last reference removes
//! it, as it does for Logi Options+'s gesture keyboard. Without a queue it
//! receives no output or feature reports, which is why its descriptor
//! declares none.

use std::ffi::CStr;
use std::ptr::NonNull;

use objc2_core_foundation::{CFData, CFDictionary, CFIndex, CFRetained, CFString, CFType};
use objc2_io_kit::{
    IOHIDUserDevice, kIOHIDProductKey, kIOHIDReportDescriptorKey, kIOReturnSuccess,
};

use super::{REPORT_LEN, Report, SendError, Sink};

/// [`REPORT_LEN`] as the `CFIndex` the report call takes.
#[expect(
    clippy::cast_possible_wrap,
    reason = "the eight-byte report length fits a CFIndex"
)]
const REPORT_LEN_INDEX: CFIndex = REPORT_LEN as CFIndex;

/// A created virtual HID device; dropping it removes the device.
pub(super) struct Device(CFRetained<IOHIDUserDevice>);

// SAFETY: `IOHIDUserDevice` is a CoreFoundation object, whose retain and
// release are thread-safe. This device is never scheduled on a run loop or a
// dispatch queue, so it has no thread affinity, and the keyboard slot's mutex
// gives one thread at a time access to it.
unsafe impl Send for Device {}

impl Device {
    /// Create a HID device with `descriptor` as its report descriptor and
    /// `product` as its product name. `None` when the system refuses, as it
    /// does for a process without the
    /// `com.apple.developer.hid.virtual.device` entitlement.
    pub(super) fn create(product: &str, descriptor: &[u8]) -> Option<Self> {
        let descriptor_key = key(kIOHIDReportDescriptorKey)?;
        let product_key = key(kIOHIDProductKey)?;
        let descriptor = CFData::from_bytes(descriptor);
        let product = CFString::from_str(product);
        let properties = CFDictionary::<CFString, CFType>::from_slices(
            &[&descriptor_key, &product_key],
            &[&descriptor, &product],
        );
        // SAFETY: every key is a `CFString` and every value has the type its
        // key documents (`CFData` for the descriptor, `CFString` for the
        // product); `None` selects the default allocator and `0` no options.
        let device = unsafe { IOHIDUserDevice::with_properties(None, properties.as_opaque(), 0) }?;
        Some(Self(device))
    }
}

impl Sink for Device {
    fn send(&self, report: Report) -> Result<(), SendError> {
        let bytes: &[u8; REPORT_LEN] = report.bytes();
        // SAFETY: reads the system's monotonic clock and has no preconditions.
        let timestamp = unsafe { mach2::mach_time::mach_absolute_time() };
        // SAFETY: `bytes` is a live buffer of `REPORT_LEN_INDEX` bytes for the
        // duration of the call, which only reads it (the C parameter is
        // `const uint8_t *`).
        let status = unsafe {
            self.0.handle_report_with_time_stamp(
                timestamp,
                NonNull::from(bytes).cast(),
                REPORT_LEN_INDEX,
            )
        };
        if status == kIOReturnSuccess {
            Ok(())
        } else {
            Err(SendError(status))
        }
    }
}

/// A HID property key as the `CFString` a properties dictionary takes.
fn key(name: &CStr) -> Option<CFRetained<CFString>> {
    name.to_str().ok().map(CFString::from_str)
}

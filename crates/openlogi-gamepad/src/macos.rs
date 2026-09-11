//! macOS `IOHIDUserDevice` backend.
//!
//! Requires the restricted entitlement `com.apple.developer.hid.virtual.device`
//! on the agent binary. Without it, [`create`] returns
//! [`GamepadError::EntitlementRequired`].
//!
//! Host rumble callbacks are deferred: the public
//! `IOHIDUserDeviceRegisterOutputReportCallback` symbol is not linkable on
//! every SDK, so [`VirtualGamepad::poll_rumble`] is currently always empty on
//! macOS. Input reports still work for the Gamepad API.

#![expect(unsafe_code, reason = "IOHIDUserDevice is a raw IOKit C API")]

use std::ffi::c_void;
use std::ptr;

use core_foundation::base::{CFAllocatorRef, CFType, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use crate::descriptor::{
    OPENLOGI_GAMEPAD_PID, OPENLOGI_GAMEPAD_VID, STANDARD_GAMEPAD_REPORT_DESCRIPTOR,
};
use crate::{GamepadError, GamepadState, Rumble, VirtualGamepad};

type IoHidUserDeviceRef = *mut c_void;
type IoReturn = i32;

const K_IO_RETURN_SUCCESS: IoReturn = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDUserDeviceCreate(
        allocator: CFAllocatorRef,
        properties: *const c_void,
    ) -> IoHidUserDeviceRef;

    fn IOHIDUserDeviceHandleReport(
        device: IoHidUserDeviceRef,
        report: *const u8,
        report_length: usize,
    ) -> IoReturn;

    fn CFRelease(cf: *const c_void);
}

struct MacGamepad {
    device: IoHidUserDeviceRef,
}

// SAFETY: IOHIDUserDevice is used from a single owner thread at a time; the
// agent serializes set_state / poll_rumble / shutdown on one worker.
unsafe impl Send for MacGamepad {}

/// Create a macOS virtual gamepad.
pub fn create(product_name: &str) -> Result<Box<dyn VirtualGamepad>, GamepadError> {
    let properties = properties_dict(product_name);
    // SAFETY: properties is a live CFDictionary; NULL allocator uses the default.
    let device = unsafe { IOHIDUserDeviceCreate(ptr::null(), properties.as_CFTypeRef().cast()) };
    if device.is_null() {
        return Err(GamepadError::EntitlementRequired);
    }

    Ok(Box::new(MacGamepad { device }))
}

fn properties_dict(product_name: &str) -> CFMutableDictionary<CFString, CFType> {
    let mut dict = CFMutableDictionary::<CFString, CFType>::new();
    dict.set(
        CFString::new("ReportDescriptor"),
        CFData::from_buffer(STANDARD_GAMEPAD_REPORT_DESCRIPTOR).as_CFType(),
    );
    dict.set(
        CFString::new("VendorID"),
        CFNumber::from(i32::try_from(OPENLOGI_GAMEPAD_VID).unwrap_or(0x1209)).as_CFType(),
    );
    dict.set(
        CFString::new("ProductID"),
        CFNumber::from(i32::try_from(OPENLOGI_GAMEPAD_PID).unwrap_or(0x0C06)).as_CFType(),
    );
    dict.set(
        CFString::new("Product"),
        CFString::new(product_name).as_CFType(),
    );
    dict.set(
        CFString::new("Transport"),
        CFString::new("Virtual").as_CFType(),
    );
    dict
}

impl VirtualGamepad for MacGamepad {
    fn set_state(&mut self, state: &GamepadState) -> Result<(), GamepadError> {
        let report = state.to_input_report();
        // SAFETY: device is a live IOHIDUserDevice; report is stack-owned.
        let status =
            unsafe { IOHIDUserDeviceHandleReport(self.device, report.as_ptr(), report.len()) };
        if status != K_IO_RETURN_SUCCESS {
            return Err(GamepadError::Io(std::io::Error::other(
                "IOHIDUserDeviceHandleReport failed",
            )));
        }
        Ok(())
    }

    fn poll_rumble(&mut self) -> Option<Rumble> {
        None
    }

    fn shutdown(self: Box<Self>) -> Result<(), GamepadError> {
        // SAFETY: balances IOHIDUserDeviceCreate.
        unsafe {
            CFRelease(self.device.cast());
        }
        Ok(())
    }
}

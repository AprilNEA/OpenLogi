//! macOS aggregate camera-use probe.
//!
//! CoreMediaIO exposes whether each camera device is running in any client.
//! Reading that property covers physical webcams, virtual cameras, capture
//! cards, and SLR devices without coupling the policy to a particular meeting
//! or recording application.
#![expect(
    unsafe_code,
    reason = "CoreMediaIO exposes the camera-running property through a C API"
)]

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;

type ObjectId = u32;
type Selector = u32;
type Scope = u32;
type Element = u32;

#[repr(C)]
struct PropertyAddress {
    selector: Selector,
    scope: Scope,
    element: Element,
}

// CoreMediaIO constants are four-character codes from CMIOTypes.h.
const SYSTEM_OBJECT: ObjectId = 1;
const SCOPE_GLOBAL: Scope = u32::from_be_bytes(*b"glob");
const ELEMENT_MASTER: Element = 0;
const HARDWARE_DEVICES: Selector = u32::from_be_bytes(*b"dev#");
const DEVICE_RUNNING_SOMEWHERE: Selector = u32::from_be_bytes(*b"gone");

#[link(name = "CoreMediaIO", kind = "framework")]
unsafe extern "C" {
    fn CMIOObjectGetPropertyDataSize(
        object_id: ObjectId,
        address: *const PropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
    ) -> i32;

    fn CMIOObjectGetPropertyData(
        object_id: ObjectId,
        address: *const PropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: u32,
        data_used: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

/// Report whether any camera device is running in any client. The error is the
/// failing `OSStatus`.
pub(super) fn camera_in_use() -> Result<bool, i32> {
    let devices_address = PropertyAddress {
        selector: HARDWARE_DEVICES,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MASTER,
    };
    let mut data_size = 0;
    // SAFETY: CoreMediaIO receives a valid system-object property address
    // and a writable UInt32 for the byte count; no qualifier is used.
    let status = unsafe {
        CMIOObjectGetPropertyDataSize(
            SYSTEM_OBJECT,
            &raw const devices_address,
            0,
            ptr::null(),
            &raw mut data_size,
        )
    };
    if status != 0 {
        return Err(status);
    }

    let object_size = size_of::<ObjectId>();
    let Some(device_count) = usize::try_from(data_size)
        .ok()
        .filter(|bytes| bytes % object_size == 0)
        .map(|bytes| bytes / object_size)
    else {
        return Err(-1);
    };
    if device_count == 0 {
        return Ok(false);
    }

    let mut devices = vec![0; device_count];
    let mut data_used = 0;
    // SAFETY: `devices` has the byte capacity reported by the preceding
    // size query and remains alive for the duration of the call.
    let status = unsafe {
        CMIOObjectGetPropertyData(
            SYSTEM_OBJECT,
            &raw const devices_address,
            0,
            ptr::null(),
            data_size,
            &raw mut data_used,
            devices.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    let Some(used_count) = usize::try_from(data_used)
        .ok()
        .filter(|bytes| bytes % object_size == 0)
        .map(|bytes| bytes / object_size)
        .filter(|count| *count <= devices.len())
    else {
        return Err(-1);
    };
    devices.truncate(used_count);

    let running_address = PropertyAddress {
        selector: DEVICE_RUNNING_SOMEWHERE,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MASTER,
    };
    let mut last_error = None;
    let mut read_any = false;
    for device in devices {
        let mut running = 0_u32;
        let property_size = u32::try_from(size_of::<u32>()).unwrap_or(u32::MAX);
        let mut property_used = 0;
        // SAFETY: `running` and the size counter are valid writable
        // buffers; each device ID came from CoreMediaIO itself.
        let status = unsafe {
            CMIOObjectGetPropertyData(
                device,
                &raw const running_address,
                0,
                ptr::null(),
                property_size,
                &raw mut property_used,
                (&raw mut running).cast(),
            )
        };
        if status != 0 {
            last_error = Some(status);
            continue;
        }
        if property_used != property_size {
            last_error = Some(-1);
            continue;
        }
        read_any = true;
        if running != 0 {
            return Ok(true);
        }
    }
    if read_any {
        Ok(false)
    } else {
        Err(last_error.unwrap_or(-1))
    }
}

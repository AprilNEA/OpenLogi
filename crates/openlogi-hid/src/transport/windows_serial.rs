//! Recover a Windows HID node's serial number from its parent device instance.
//!
//! `HidD_GetSerialNumberString` reads the USB string descriptor the *HID
//! interface* exposes, and a device is free to expose none: a Litra Glow
//! answers it successfully with an empty string, as it does for the product
//! string. The serial is not missing from the system, only from that call —
//! Windows records it while enumerating the USB node above the HID collections,
//! so `HID\VID_046D&PID_C900&COL02\...` has the parent instance
//! `USB\VID_046D&PID_C900\GLOWSERIAL01`.
//!
//! Without it such a device has no stable identity, so every setting saved
//! against it — a Litra's brightness, or whether its power follows the camera —
//! lives only as long as the process that wrote it.
#![expect(
    unsafe_code,
    reason = "the Configuration Manager exposes the device tree through a C API"
)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Parent, CM_Locate_DevNodeW, CR_SUCCESS, MAX_DEVICE_ID_LEN,
};

/// The device-interface class GUID suffix and the `\\?\` prefix that wrap an
/// instance id inside an interface path.
const INTERFACE_PATH_PREFIX: &str = r"\\?\";

/// Serial recovered from the parent of the HID node at `interface_path`, if
/// that parent carries a real one.
pub(super) fn serial_from_parent(interface_path: &str) -> Option<String> {
    let instance_id = instance_id_from_interface_path(interface_path)?;
    let parent = parent_device_id(&instance_id)?;
    device_serial(&parent)
}

/// Turn a device-interface path into the device instance id it names.
///
/// The two are the same string in different clothes: the path adds the `\\?\`
/// prefix and the interface class GUID, and writes the separators as `#`.
fn instance_id_from_interface_path(path: &str) -> Option<String> {
    let path = path.strip_prefix(INTERFACE_PATH_PREFIX).unwrap_or(path);
    // The trailing `{...}` is the interface class, not part of the instance.
    let instance = path.split_once("#{").map_or(path, |(instance, _)| instance);
    (!instance.is_empty()).then(|| instance.replace('#', "\\"))
}

/// Device id of `instance_id`'s parent devnode.
fn parent_device_id(instance_id: &str) -> Option<String> {
    let wide: Vec<u16> = instance_id
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut devnode = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call,
    // and `devnode` is a writable u32 the API fills on success.
    let status = unsafe { CM_Locate_DevNodeW(&raw mut devnode, wide.as_ptr(), 0) };
    if status != CR_SUCCESS {
        return None;
    }

    let mut parent = 0;
    // SAFETY: `devnode` was just filled by CM_Locate_DevNodeW, and `parent` is
    // a writable u32 the API fills on success.
    let status = unsafe { CM_Get_Parent(&raw mut parent, devnode, 0) };
    if status != CR_SUCCESS {
        return None;
    }

    // MAX_DEVICE_ID_LEN counts characters and excludes the terminator.
    let mut buffer = [0_u16; MAX_DEVICE_ID_LEN as usize + 1];
    let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: `parent` came from CM_Get_Parent, and `buffer` has the capacity
    // reported to the call.
    let status = unsafe { CM_Get_Device_IDW(parent, buffer.as_mut_ptr(), capacity, 0) };
    if status != CR_SUCCESS {
        return None;
    }

    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(
        OsString::from_wide(&buffer[..end])
            .to_string_lossy()
            .into_owned(),
    )
}

/// The serial in a device id, when the device actually reported one.
///
/// A device id's last segment is the device's own serial when it has one, and
/// a Windows-generated instance number otherwise. The two are told apart by
/// `&`: the generated form always contains it (`b&1234abcd&0&0001`), while a
/// serial descriptor may not contain one at all — USB-IF requires the string to
/// be unique, and Windows rejects a descriptor containing `&` rather than
/// storing it here.
fn device_serial(device_id: &str) -> Option<String> {
    let candidate = device_id.rsplit('\\').next()?;
    (!candidate.is_empty() && !candidate.contains('&')).then(|| candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{device_serial, instance_id_from_interface_path};

    #[test]
    fn interface_path_yields_the_instance_id_it_wraps() {
        assert_eq!(
            instance_id_from_interface_path(
                r"\\?\HID#VID_046D&PID_C900&Col02#b&1234abcd&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}"
            )
            .as_deref(),
            Some(r"HID\VID_046D&PID_C900&Col02\b&1234abcd&0&0001")
        );
    }

    #[test]
    fn a_path_without_the_interface_guid_is_still_an_instance_id() {
        assert_eq!(
            instance_id_from_interface_path(r"\\?\HID#VID_046D&PID_C900&Col02#b&1234abcd&0&0001")
                .as_deref(),
            Some(r"HID\VID_046D&PID_C900&Col02\b&1234abcd&0&0001")
        );
    }

    #[test]
    fn a_device_reported_serial_is_taken() {
        assert_eq!(
            device_serial(r"USB\VID_046D&PID_C900\GLOWSERIAL01").as_deref(),
            Some("GLOWSERIAL01")
        );
    }

    #[test]
    fn a_windows_generated_instance_number_is_not_a_serial() {
        // The shape Windows invents for a device that reports no serial. Taking
        // it would mint an identity that moves with the USB port.
        assert!(device_serial(r"USB\VID_046D&PID_C53F\5&2b3c1d&0&3").is_none());
        assert!(device_serial(r"HID\VID_046D&PID_C900&COL02\b&1234abcd&0&0001").is_none());
    }

    #[test]
    fn a_device_id_without_a_last_segment_yields_nothing() {
        assert!(device_serial("").is_none());
        assert!(device_serial(r"USB\VID_046D&PID_C900\").is_none());
    }
}

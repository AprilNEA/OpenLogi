//! Linux input-device access probes.
//!
//! There is no privacy-consent database here: access is granted by the udev
//! rules that put `/dev/uinput` and the Logitech `/dev/hidraw*` nodes in reach
//! of the user, so each probe is an open() that is immediately dropped.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::PermissionStatus;

const LOGITECH_VID: u32 = 0x046d;

/// Probe Linux input-device access: `/dev/uinput` (write) and at least one
/// Logitech `/dev/hidraw*` (read/write).
///
/// Returns:
/// - `Granted` — both uinput and at least one Logitech hidraw are accessible.
/// - `Denied` — uinput is inaccessible, or a Logitech hidraw exists but is
///   inaccessible.
/// - `Unknown` — uinput is accessible but no Logitech hidraw device is
///   currently connected (nothing to report yet).
#[must_use]
pub fn input_device_access() -> PermissionStatus {
    classify(probe_uinput(), probe_logitech_hidraw())
}

/// Pure classification logic, factored out so it is testable without device nodes.
///
/// - `uinput_ok`: whether `/dev/uinput` is writable.
/// - `hidraw_ok`: `Some(true)` = Logitech hidraw accessible, `Some(false)` =
///   Logitech hidraw present but not accessible, `None` = no Logitech hidraw
///   present at all.
pub(crate) fn classify(uinput_ok: bool, hidraw_ok: Option<bool>) -> PermissionStatus {
    match (uinput_ok, hidraw_ok) {
        (true, Some(true)) => PermissionStatus::Granted,
        (false, _) | (_, Some(false)) => PermissionStatus::Denied,
        (true, None) => PermissionStatus::Unknown,
    }
}

/// Try to open `/dev/uinput` for writing. No data is written; we just check
/// whether the open succeeds (permission granted) or fails with EACCES/EPERM.
/// NotFound (uinput module not loaded) is also treated as inaccessible.
fn probe_uinput() -> bool {
    fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// Probe Logitech hidraw devices.
///
/// Returns:
/// - `Some(true)` — at least one Logitech hidraw is present and accessible.
/// - `Some(false)` — at least one Logitech hidraw is present but permission
///   is denied.
/// - `None` — no Logitech hidraw device found (nothing connected).
fn probe_logitech_hidraw() -> Option<bool> {
    let mut any_accessible = false;
    let mut any_denied = false;

    // Iterate lazily; `any_accessible` short-circuits after first success.
    for entry in fs::read_dir("/dev").ok()?.filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with("hidraw") || !is_logitech_hidraw(&name) {
            continue;
        }
        match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new("/dev").join(&name))
        {
            Ok(_) => {
                any_accessible = true;
                break; // one accessible device is enough
            }
            Err(e) if matches!(e.kind(), ErrorKind::PermissionDenied) => any_denied = true,
            Err(_) => {} // device gone or other transient error — skip
        }
    }

    if any_accessible {
        Some(true)
    } else if any_denied {
        Some(false)
    } else {
        None
    }
}

/// Check whether a hidraw device belongs to Logitech by reading the HID_ID
/// field from its sysfs uevent file.
///
/// The uevent file contains a line like `HID_ID=0003:0000046D:0000C52B`
/// (bus : vendor : product, each zero-padded to 8 hex digits). We compare
/// the vendor field numerically so `0000046D` and `046d` both match.
fn is_logitech_hidraw(hidraw_name: &str) -> bool {
    let uevent_path = format!("/sys/class/hidraw/{hidraw_name}/device/uevent");
    let Ok(contents) = fs::read_to_string(&uevent_path) else {
        return false;
    };
    contents.lines().any(|line| {
        // HID_ID=<bus>:<vendor>:<product>
        line.starts_with("HID_ID=")
            && line
                .split(':')
                .nth(1)
                .and_then(|vendor| u32::from_str_radix(vendor.trim(), 16).ok())
                .is_some_and(|vid| vid == LOGITECH_VID)
    })
}

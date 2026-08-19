//! Linux input-device access probes.
//!
//! There is no privacy-consent database here: access comes from the udev rules
//! that put `/dev/uinput` and the Logitech `/dev/hidraw*` nodes in reach of the
//! user, so each probe is an `open()` that is immediately dropped.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use openlogi_core::hid::LOGITECH_VENDOR_ID;

use crate::PermissionStatus;

/// Probe Linux input-device access: `/dev/uinput` (write) and at least one
/// Logitech `/dev/hidraw*` (read/write).
///
/// - `Granted` — both are accessible.
/// - `Denied` — uinput is inaccessible, or a Logitech hidraw is present but is
///   not.
/// - `Unknown` — uinput is fine but no Logitech hidraw is connected.
#[must_use]
pub fn input_device_access() -> PermissionStatus {
    classify(probe_uinput(), probe_logitech_hidraw())
}

/// Split from the probes so it is testable without device nodes.
pub(crate) fn classify(uinput_ok: bool, hidraw_ok: Option<bool>) -> PermissionStatus {
    match (uinput_ok, hidraw_ok) {
        (true, Some(true)) => PermissionStatus::Granted,
        (false, _) | (_, Some(false)) => PermissionStatus::Denied,
        (true, None) => PermissionStatus::Unknown,
    }
}

/// Is `/dev/uinput` writable? NotFound (module not loaded) counts as no.
fn probe_uinput() -> bool {
    fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// `Some(true)` — a Logitech hidraw is accessible; `Some(false)` — one is
/// present but permission is denied; `None` — none connected.
fn probe_logitech_hidraw() -> Option<bool> {
    let mut any_accessible = false;
    let mut any_denied = false;

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

/// Match a hidraw's sysfs `uevent` line `HID_ID=0003:0000046D:0000C52B`
/// (bus : vendor : product) against the Logitech vendor ID — numerically, so
/// `0000046D` and `046d` both match.
fn is_logitech_hidraw(hidraw_name: &str) -> bool {
    let uevent_path = format!("/sys/class/hidraw/{hidraw_name}/device/uevent");
    let Ok(contents) = fs::read_to_string(&uevent_path) else {
        return false;
    };
    contents.lines().any(|line| {
        line.starts_with("HID_ID=")
            && line
                .split(':')
                .nth(1)
                .and_then(|vendor| u16::from_str_radix(vendor.trim(), 16).ok())
                .is_some_and(|vid| vid == LOGITECH_VENDOR_ID)
    })
}

//! Linux input-device access probes.
//!
//! There is no privacy-consent database here: access comes from the udev rules
//! that put `/dev/uinput`, the Logitech `/dev/hidraw*` nodes, and those devices'
//! `/dev/input/event*` nodes in reach of the user, so each probe is an `open()`
//! that is immediately dropped.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use openlogi_core::hid::LOGITECH_VENDOR_ID;

use crate::PermissionStatus;

/// Probe Linux input-device access: `/dev/uinput` (write), at least one
/// Logitech `/dev/hidraw*` (read/write), and those devices' `/dev/input/event*`
/// nodes (read).
///
/// All three matter, and the third is not implied by the other two. HID++ rides
/// hidraw, so DPI and SmartShift work off that alone, while button remapping
/// needs the hook to read the mouse's event node — which `logind` does not grant
/// on its own for a Bluetooth-direct mouse, whose node hangs off
/// `/devices/virtual/misc/uhid` and has no seat. Leftover rules from another
/// Logitech manager typically cover hidraw and uinput but not that, so reporting
/// on two probes called such a system `Granted` while remapping silently did
/// nothing.
///
/// - `Granted` — all three are accessible.
/// - `Denied` — any one of them is present but inaccessible.
/// - `Unknown` — uinput is fine but no Logitech hidraw is connected.
#[must_use]
pub fn input_device_access() -> PermissionStatus {
    classify(
        probe_uinput(),
        probe_logitech_hidraw(),
        probe_logitech_event(),
    )
}

/// Split from the probes so it is testable without device nodes.
///
/// A `None` event probe does not block `Granted`: it means no Logitech event
/// node was found at all, which on a machine with an accessible hidraw points at
/// sysfs being unreadable rather than at a permission problem. Only a definite
/// denial downgrades, so a working setup can never be reported as broken.
pub(crate) fn classify(
    uinput_ok: bool,
    hidraw_ok: Option<bool>,
    event_ok: Option<bool>,
) -> PermissionStatus {
    match (uinput_ok, hidraw_ok, event_ok) {
        (false, _, _) | (_, Some(false), _) | (_, _, Some(false)) => PermissionStatus::Denied,
        (true, Some(true), _) => PermissionStatus::Granted,
        (true, None, _) => PermissionStatus::Unknown,
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

/// `Some(true)` — a Logitech `/dev/input/event*` node is readable; `Some(false)`
/// — one is present but permission is denied; `None` — none found.
///
/// Read access is what the hook needs: it enumerates event nodes, keeps the
/// Logitech mice, and grabs them exclusively. `evdev::enumerate` silently skips
/// any node it cannot open, so a denied node does not surface as an error — the
/// mouse simply never appears and remapping does nothing.
///
/// Every event node of a Logitech device counts, not just the mouse's. Telling
/// them apart means decoding the sysfs capability bitmasks, and it would buy
/// nothing: `uaccess` is granted per node by the same rule, so they stand or
/// fall together.
fn probe_logitech_event() -> Option<bool> {
    let mut any_accessible = false;
    let mut any_denied = false;

    for entry in fs::read_dir("/dev/input").ok()?.filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with("event") || !is_logitech_event(&name) {
            continue;
        }
        match fs::OpenOptions::new().read(true).open(entry.path()) {
            Ok(_) => {
                any_accessible = true;
                break; // one accessible node is enough
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

/// Match an event node's sysfs `device/id/vendor` (four hex digits, e.g. `046d`)
/// against the Logitech vendor ID.
fn is_logitech_event(event_name: &str) -> bool {
    let vendor_path = format!("/sys/class/input/{event_name}/device/id/vendor");
    fs::read_to_string(&vendor_path)
        .ok()
        .and_then(|vendor| u16::from_str_radix(vendor.trim(), 16).ok())
        .is_some_and(|vid| vid == LOGITECH_VENDOR_ID)
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

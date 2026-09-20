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
    Probes {
        uinput_writable: probe_uinput(),
        hidraw: probe_logitech_hidraw(),
    }
    .into()
}

/// What probing the Logitech `/dev/hidraw*` nodes established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HidrawProbe {
    /// At least one Logitech hidraw opened read/write.
    Accessible,
    /// A Logitech hidraw is present but `open()` was refused.
    Denied,
    /// No Logitech hidraw is connected to probe.
    NonePresent,
}

/// Both probes, taken together.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Probes {
    /// `/dev/uinput` opened for writing.
    pub(crate) uinput_writable: bool,
    pub(crate) hidraw: HidrawProbe,
}

/// The verdict is a total function of the two probes — split from the
/// probing itself so it is testable without device nodes.
impl From<Probes> for PermissionStatus {
    fn from(probes: Probes) -> Self {
        match (probes.uinput_writable, probes.hidraw) {
            (true, HidrawProbe::Accessible) => Self::Granted,
            (false, _) | (_, HidrawProbe::Denied) => Self::Denied,
            (true, HidrawProbe::NonePresent) => Self::Unknown,
        }
    }
}

/// Is `/dev/uinput` writable? NotFound (module not loaded) counts as no.
fn probe_uinput() -> bool {
    fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

fn probe_logitech_hidraw() -> HidrawProbe {
    let mut any_denied = false;

    let Ok(entries) = fs::read_dir("/dev") else {
        return HidrawProbe::NonePresent;
    };
    for entry in entries.filter_map(Result::ok) {
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
            Ok(_) => return HidrawProbe::Accessible, // one accessible device is enough
            Err(e) if matches!(e.kind(), ErrorKind::PermissionDenied) => any_denied = true,
            Err(_) => {} // device gone or other transient error — skip
        }
    }

    if any_denied {
        HidrawProbe::Denied
    } else {
        HidrawProbe::NonePresent
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

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
/// Only the nodes the hook would actually grab count, and among those one denial
/// outweighs any number of readable siblings — the opposite of
/// [`probe_logitech_hidraw`], which stops at the first device it can open.
///
/// The asymmetry is the point. HID++ needs some route to the device, so one
/// reachable hidraw really is enough. The hook needs *each* mouse's own event
/// node, so stopping at the first readable one would let one mouse mask another
/// whose node is denied — and which is seen first is only filesystem order.
///
/// The capability filter is not optional. A single Logitech device publishes
/// several event nodes, and the non-pointer ones legitimately have no `uaccess`
/// ACL: a G502 exposes `Logitech G502 HERO Gaming Mouse Keyboard` alongside the
/// mouse, root-owned, on a correctly configured machine. Counting every node of
/// a Logitech device would report `Denied` for that, which is a working system.
/// [`is_hookable_logitech_mouse`] applies the same `clicks && moves` test the
/// hook's own `is_hookable_mouse` does.
fn probe_logitech_event() -> Option<bool> {
    let outcomes = fs::read_dir("/dev/input")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .into_string()
                .is_ok_and(|name| name.starts_with("event") && is_hookable_logitech_mouse(&name))
        })
        .map(|entry| {
            fs::OpenOptions::new()
                .read(true)
                .open(entry.path())
                .map(|_| ())
                .map_err(|e| e.kind())
        });
    fold_event_access(outcomes)
}

/// Fold each node's open outcome into the tri-state, denial first.
///
/// Split from [`probe_logitech_event`] so the precedence rule is testable
/// without device nodes — in particular that it does not depend on the order
/// the nodes happen to be walked in.
///
/// Anything other than `PermissionDenied` (a device unplugged mid-walk, say) is
/// neither evidence of access nor of its absence, so it is ignored.
pub(crate) fn fold_event_access(
    outcomes: impl IntoIterator<Item = Result<(), ErrorKind>>,
) -> Option<bool> {
    let mut any_accessible = false;
    let mut any_denied = false;

    for outcome in outcomes {
        match outcome {
            Ok(()) => any_accessible = true,
            Err(ErrorKind::PermissionDenied) => any_denied = true,
            Err(_) => {}
        }
    }

    if any_denied {
        Some(false)
    } else if any_accessible {
        Some(true)
    } else {
        None
    }
}

/// Is this event node a Logitech mouse the hook would grab?
///
/// Vendor comes from sysfs `device/id/vendor` (four hex digits, e.g. `046d`);
/// the rest mirrors the hook's `is_hookable_mouse`, which needs a device that
/// both clicks and moves relatively. The combo-device exclusions it also applies
/// are omitted deliberately: they decide whether grabbing is *safe*, while this
/// only needs to know which nodes are candidates at all.
fn is_hookable_logitech_mouse(event_name: &str) -> bool {
    let attr = |name: &str| {
        fs::read_to_string(format!("/sys/class/input/{event_name}/device/{name}")).ok()
    };
    let logitech = attr("id/vendor")
        .and_then(|vendor| u16::from_str_radix(vendor.trim(), 16).ok())
        .is_some_and(|vid| vid == LOGITECH_VENDOR_ID);
    if !logitech {
        return false;
    }
    let moves = attr("capabilities/rel").is_some_and(|rel| {
        has_capability_bit(&rel, REL_X_BIT) && has_capability_bit(&rel, REL_Y_BIT)
    });
    let clicks = attr("capabilities/key").is_some_and(|key| has_capability_bit(&key, BTN_LEFT_BIT));
    moves && clicks
}

/// `REL_X` / `REL_Y` and `BTN_LEFT` as their kernel event codes, the bit
/// positions sysfs capability bitmaps use.
const REL_X_BIT: usize = 0x00;
const REL_Y_BIT: usize = 0x01;
const BTN_LEFT_BIT: usize = 0x110;

/// Is `bit` set in a sysfs capability bitmap?
///
/// The kernel prints these as space-separated hex words, most significant
/// first — `ffff0000 0 0 0 0` sets `BTN_LEFT` (bit 0x110) in the fifth word from
/// the end. Reversing gives word `bit / 64` at its natural index.
pub(crate) fn has_capability_bit(bitmap: &str, bit: usize) -> bool {
    bitmap
        .split_whitespace()
        .rev()
        .nth(bit / u64::BITS as usize)
        .and_then(|word| u64::from_str_radix(word, 16).ok())
        .is_some_and(|word| word >> (bit % u64::BITS as usize) & 1 == 1)
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

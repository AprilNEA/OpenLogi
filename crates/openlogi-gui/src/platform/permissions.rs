//! Privacy-permission status for the Settings window.
//!
//! ## macOS
//!
//! OpenLogi needs two real permissions: **Accessibility** (for the gesture /
//! button hook's event tap) and **Input Monitoring** (to open HID devices via
//! `IOHIDManager`). **Bluetooth** (CoreBluetooth) is surfaced for completeness;
//! OpenLogi reaches BLE mice via `IOHIDManager`, not CoreBluetooth, so it
//! usually reads [`PermissionStatus::Unknown`].
//!
//! Accessibility status is owned by [`crate::state::AppState`] (the
//! accessibility watcher keeps it live); this module covers the other two plus
//! the System-Settings deep links.
//!
//! ## Linux
//!
//! The platform permission model is based on device-file access rather than
//! privacy-consent dialogs. OpenLogi needs:
//! - **Write access to `/dev/uinput`** — to create virtual input devices for
//!   the evdev/uinput hook.
//! - **Read/write access to `/dev/hidraw*`** — to communicate with the Logitech
//!   Bolt receiver or directly-connected devices over HID++.
//!
//! Both are granted by installing the OpenLogi udev rules (see the Linux
//! install guide).

/// Tri-state result of a permission query.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermissionStatus {
    /// The app may use the capability.
    Granted,
    /// The user denied it (or it's restricted).
    Denied,
    /// Not yet determined, or the platform can't report a definite state.
    Unknown,
}

/// A privacy permission with a platform action (deep-link or install guide).
#[derive(Clone, Copy)]
pub enum Permission {
    /// macOS: Accessibility (event tap for button remapping).
    Accessibility,
    /// macOS: Input Monitoring (HID device access via IOHIDManager).
    #[cfg(target_os = "macos")]
    InputMonitoring,
    /// macOS: CoreBluetooth authorization.
    #[cfg(target_os = "macos")]
    Bluetooth,
}

/// Current Input Monitoring ("listen event") status.
#[cfg(target_os = "macos")]
#[must_use]
pub fn input_monitoring() -> PermissionStatus {
    macos::input_monitoring()
}

/// Current CoreBluetooth authorization status.
#[cfg(target_os = "macos")]
#[must_use]
pub fn bluetooth() -> PermissionStatus {
    macos::bluetooth()
}

/// Probe Linux input-device access: `/dev/uinput` (write) and at least one
/// Logitech `/dev/hidraw*` (read/write).
///
/// Returns:
/// - `Granted` — both uinput and at least one Logitech hidraw are accessible.
/// - `Denied` — uinput is inaccessible, or a Logitech hidraw exists but is
///   inaccessible.
/// - `Unknown` — uinput is accessible but no Logitech hidraw device is
///   currently connected (nothing to report yet).
#[cfg(target_os = "linux")]
#[must_use]
pub fn input_device_access() -> PermissionStatus {
    let uinput_ok = linux::probe_uinput();
    let hidraw_ok = linux::probe_logitech_hidraw();
    classify(uinput_ok, hidraw_ok)
}

/// Pure classification logic, factored out so it is testable without device nodes.
///
/// - `uinput_ok`: whether `/dev/uinput` is writable.
/// - `hidraw_ok`: `Some(true)` = Logitech hidraw accessible, `Some(false)` =
///   Logitech hidraw present but not accessible, `None` = no Logitech hidraw
///   present at all.
#[cfg(target_os = "linux")]
pub(crate) fn classify(uinput_ok: bool, hidraw_ok: Option<bool>) -> PermissionStatus {
    match (uinput_ok, hidraw_ok) {
        (true, Some(true)) => PermissionStatus::Granted,
        (false, _) | (_, Some(false)) => PermissionStatus::Denied,
        (true, None) => PermissionStatus::Unknown,
    }
}

/// Open the platform-specific remediation pane / guide for `permission`.
///
/// On macOS this opens the relevant System Settings privacy pane. It
/// deliberately does **not** fire the Accessibility prompt — the agent owns the
/// CGEventTap, so the prompt must run in the agent process (see
/// [`crate::state::AppState::request_accessibility_prompt`]).
///
/// On Linux this is currently a no-op (the install guide is shown inline in the
/// Settings window description text).
#[cfg(target_os = "macos")]
pub fn open_pane(permission: Permission) {
    let anchor = match permission {
        Permission::Accessibility => "Privacy_Accessibility",
        Permission::InputMonitoring => "Privacy_ListenEvent",
        Permission::Bluetooth => "Privacy_Bluetooth",
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    if let Err(e) = std::process::Command::new("open").arg(&url).spawn() {
        tracing::warn!(error = %e, url, "could not open System Settings");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_pane(_permission: Permission) {}

// ── macOS FFI ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    #![expect(
        unsafe_code,
        reason = "IOKit (IOHIDCheckAccess) + CoreBluetooth privacy-permission FFI"
    )]

    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    use super::PermissionStatus;

    // Query the current HID access without prompting. `IOHIDRequestType`:
    // PostEvent = 0, ListenEvent = 1. Returned `IOHIDAccessType`: Granted = 0,
    // Denied = 1, Unknown = 2.
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDCheckAccess(request_type: u32) -> u32;
    }

    // Force-link CoreBluetooth so the `CBCentralManager` class is normally
    // registered for the `Class::get` lookup in `bluetooth()` (which degrades
    // to `Unknown` rather than panicking if it somehow isn't).
    #[link(name = "CoreBluetooth", kind = "framework")]
    unsafe extern "C" {}

    const REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

    pub(super) fn input_monitoring() -> PermissionStatus {
        // SAFETY: `IOHIDCheckAccess` is a side-effect-free query taking a valid
        // `IOHIDRequestType` discriminant.
        match unsafe { IOHIDCheckAccess(REQUEST_TYPE_LISTEN_EVENT) } {
            0 => PermissionStatus::Granted,
            1 => PermissionStatus::Denied,
            _ => PermissionStatus::Unknown,
        }
    }

    pub(super) fn bluetooth() -> PermissionStatus {
        // `+[CBManager authorization]` (inherited by CBCentralManager) is a
        // class method returning `CBManagerAuthorization`: notDetermined = 0,
        // restricted = 1, denied = 2, allowedAlways = 3. Use `AnyClass::get`
        // (not the `class!` macro) so a missing class degrades to `Unknown`
        // instead of panicking.
        let Some(cls) = AnyClass::get(c"CBCentralManager") else {
            return PermissionStatus::Unknown;
        };
        // SAFETY: sending a documented class method (`+authorization`) that
        // returns a `CBManagerAuthorization` NSInteger.
        let authorization: isize = unsafe { msg_send![cls, authorization] };
        match authorization {
            3 => PermissionStatus::Granted,
            1 | 2 => PermissionStatus::Denied,
            _ => PermissionStatus::Unknown,
        }
    }
}

// ── Linux probes ───────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;

    const LOGITECH_VID: u32 = 0x046d;

    /// Try to open `/dev/uinput` for writing. No data is written; we just check
    /// whether the open succeeds (permission granted) or fails with EACCES/EPERM.
    /// NotFound (uinput module not loaded) is also treated as inaccessible.
    pub(crate) fn probe_uinput() -> bool {
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
    pub(crate) fn probe_logitech_hidraw() -> Option<bool> {
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
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn classify_granted_when_both_ok() {
        assert_eq!(classify(true, Some(true)), PermissionStatus::Granted);
    }

    #[test]
    fn classify_denied_when_uinput_not_ok() {
        assert_eq!(classify(false, Some(true)), PermissionStatus::Denied);
        assert_eq!(classify(false, Some(false)), PermissionStatus::Denied);
        assert_eq!(classify(false, None), PermissionStatus::Denied);
    }

    #[test]
    fn classify_denied_when_hidraw_denied() {
        assert_eq!(classify(true, Some(false)), PermissionStatus::Denied);
    }

    #[test]
    fn classify_unknown_when_no_logitech_device_connected() {
        assert_eq!(classify(true, None), PermissionStatus::Unknown);
    }
}

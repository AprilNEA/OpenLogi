//! macOS privacy-permission status for the Settings window.
//!
//! OpenLogi needs two real permissions on macOS: **Accessibility** (for the
//! gesture / button hook's event tap) and **Input Monitoring** (to open HID
//! devices — including Bluetooth-LE-direct mice — through `IOHIDManager`). The
//! **Bluetooth** (CoreBluetooth) authorization is surfaced for completeness;
//! note OpenLogi reaches BLE mice via `IOHIDManager`, not CoreBluetooth, so it
//! usually reads [`PermissionStatus::Unknown`] (not determined).
//!
//! Accessibility status is owned by [`crate::state::AppState`] (the
//! accessibility watcher keeps it live); this module covers the other two plus
//! the System-Settings deep links.

/// Tri-state result of a permission query.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermissionStatus {
    /// The app may use the capability.
    Granted,
    /// The user denied it (or it's restricted).
    Denied,
    /// Not yet determined, or the platform can't report a definite state.
    Unknown,
}

/// A privacy permission with a System Settings pane.
#[derive(Clone, Copy)]
pub enum Permission {
    Accessibility,
    InputMonitoring,
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

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn input_monitoring() -> PermissionStatus {
    PermissionStatus::Unknown
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn bluetooth() -> PermissionStatus {
    PermissionStatus::Unknown
}

/// Open the System Settings privacy pane for `permission`.
///
/// This only opens the deep link — it deliberately does **not** fire the
/// Accessibility prompt. The agent owns the CGEventTap, so the prompt has to
/// run in the agent process (see [`crate::state::AppState::request_accessibility_prompt`]);
/// prompting here would authorize the GUI, the wrong binary.
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

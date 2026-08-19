//! macOS permission reads and the System-Settings deep links.
//!
//! Every query here is the *non-prompting* variant of its API pair
//! (`IOHIDCheckAccess` rather than `IOHIDRequestAccess`, `+[CBManager
//! authorization]` rather than instantiating a central manager). Whoever owns
//! the resource raises the prompt; see the crate docs.
#![expect(
    unsafe_code,
    reason = "CoreBluetooth force-link + `+[CBManager authorization]` class-method send"
)]

use objc2::msg_send;
use objc2::runtime::AnyClass;
use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestType};

use crate::{Permission, PermissionStatus};

// Force-link CoreBluetooth so the `CBCentralManager` class is normally
// registered for the `Class::get` lookup in `bluetooth()` (which degrades
// to `Unknown` rather than panicking if it somehow isn't).
#[link(name = "CoreBluetooth", kind = "framework")]
unsafe extern "C" {}

/// Current Input Monitoring ("listen event") status.
#[must_use]
pub fn input_monitoring() -> PermissionStatus {
    // `IOHIDCheckAccess` queries the current HID access without prompting;
    // `IOHIDRequestAccess` is the prompting variant we deliberately don't
    // call here (the agent owns HID I/O, so it must raise the prompt).
    match IOHIDCheckAccess(IOHIDRequestType::ListenEvent) {
        IOHIDAccessType::Granted => PermissionStatus::Granted,
        IOHIDAccessType::Denied => PermissionStatus::Denied,
        _ => PermissionStatus::Unknown,
    }
}

/// Current CoreBluetooth authorization status.
#[must_use]
pub fn bluetooth() -> PermissionStatus {
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

/// Current Camera (AVFoundation) authorization status. Delegated to
/// `openlogi-camera`, which owns all the camera FFI so the GUI doesn't
/// duplicate the AVFoundation calls.
#[must_use]
pub fn camera() -> PermissionStatus {
    match openlogi_camera::camera_authorization() {
        openlogi_camera::CameraAuthorization::Granted => PermissionStatus::Granted,
        openlogi_camera::CameraAuthorization::Denied => PermissionStatus::Denied,
        openlogi_camera::CameraAuthorization::Undetermined => PermissionStatus::Unknown,
    }
}

/// Open the System Settings privacy pane for `permission`.
///
/// This deliberately does **not** fire the Accessibility prompt — the agent
/// owns the CGEventTap, so the prompt must run in the agent process. For
/// Accessibility the pane is all this crate offers.
///
/// Note that Accessibility bypasses `tccd`'s generic consent sheet — the
/// request logs *"does not allow prompting"* — and is handled by
/// `universalAccessAuthWarn` instead, which normally pre-creates an unchecked
/// row. When that row is missing, the usual cause is a stale record whose
/// Designated Requirement no longer matches the current signature; see
/// `crates/openlogi-desktop/src/platform/AGENTS.md`.
pub fn open_pane(permission: Permission) {
    let anchor = match permission {
        Permission::Accessibility => "Privacy_Accessibility",
        Permission::InputMonitoring => "Privacy_ListenEvent",
        Permission::Bluetooth => "Privacy_Bluetooth",
        Permission::Camera => "Privacy_Camera",
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    if let Err(e) = opener::open(&url) {
        tracing::warn!(error = %e, url, "could not open System Settings");
    }
}

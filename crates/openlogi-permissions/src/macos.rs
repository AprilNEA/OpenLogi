//! macOS permission reads and the System-Settings deep links.
//!
//! Every query is the *non-prompting* variant of its API pair
//! (`IOHIDCheckAccess`, not `IOHIDRequestAccess`); whoever owns the resource
//! raises the prompt.
#![expect(
    unsafe_code,
    reason = "CoreBluetooth force-link + `+[CBManager authorization]` class-method send"
)]

use objc2::msg_send;
use objc2::runtime::AnyClass;
use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestType};

use crate::{Permission, PermissionStatus};

// Force-link CoreBluetooth so `CBCentralManager` is registered for the lookup
// in `bluetooth()`.
#[link(name = "CoreBluetooth", kind = "framework")]
unsafe extern "C" {}

/// Current Input Monitoring ("listen event") status.
#[must_use]
pub fn input_monitoring() -> PermissionStatus {
    match IOHIDCheckAccess(IOHIDRequestType::ListenEvent) {
        IOHIDAccessType::Granted => PermissionStatus::Granted,
        IOHIDAccessType::Denied => PermissionStatus::Denied,
        _ => PermissionStatus::Unknown,
    }
}

/// Current CoreBluetooth authorization status.
#[must_use]
pub fn bluetooth() -> PermissionStatus {
    // `CBManagerAuthorization`: notDetermined 0, restricted 1, denied 2,
    // allowedAlways 3. `AnyClass::get` rather than the `class!` macro so a
    // missing class degrades to `Unknown` instead of panicking.
    let Some(cls) = AnyClass::get(c"CBCentralManager") else {
        return PermissionStatus::Unknown;
    };
    // SAFETY: `+[CBManager authorization]` is a documented class method
    // returning a `CBManagerAuthorization` NSInteger.
    let authorization: isize = unsafe { msg_send![cls, authorization] };
    match authorization {
        3 => PermissionStatus::Granted,
        1 | 2 => PermissionStatus::Denied,
        _ => PermissionStatus::Unknown,
    }
}

/// Current Camera (AVFoundation) authorization status, via `openlogi-camera`,
/// which owns the camera FFI.
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
/// For Accessibility the pane is all this offers — the agent owns the
/// CGEventTap, so the prompt must run there. When the row is missing from the
/// list entirely, see the TCC rules in
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

//! Privacy-permission status, and the deep links that take a user to the
//! system UI for fixing it.
//!
//! **Reading a status never prompts.** Prompting belongs to whichever process
//! owns the resource: the agent raises Accessibility, Input Monitoring, and
//! Bluetooth prompts because it owns the event tap, HID opens, and the
//! CoreBluetooth identity. A prompt from the wrong process records the grant
//! against the wrong code-signing identity (issue #214), so this crate exposes
//! only the non-prompting half plus [`open_pane`] — which is also why no
//! general-purpose macOS permission crate fits: they assume one app asking for
//! itself.
//!
//! ## macOS
//!
//! Three agent permissions matter: **Accessibility** (the hook's event tap),
//! **Input Monitoring** (opening HID devices via `IOHIDManager`), and
//! **Bluetooth** (CoreBluetooth authorization so macOS can name OpenLogi
//! Agent). The GUI must read those three statuses from the agent over IPC,
//! never by querying this process. The macOS-only `bluetooth` helper still
//! reports *this* process's CoreBluetooth state for diagnostics; it is not
//! the agent's grant.
//!
//! Accessibility status is not read here: the agent owns the tap, so
//! `openlogi_hook::has_accessibility` is the source of truth.
//!
//! ## Linux
//!
//! Access is device-file permissions rather than consent dialogs: write to
//! `/dev/uinput` (the evdev/uinput hook's virtual devices) and read/write to
//! `/dev/hidraw*` (HID++ to the Bolt receiver or a direct connection). Both
//! come from the OpenLogi udev rules.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

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
    /// macOS: Camera (AVFoundation) authorization for the webcam preview.
    #[cfg(target_os = "macos")]
    Camera,
}

#[cfg(target_os = "macos")]
pub use macos::{bluetooth, camera, input_monitoring, open_pane};

#[cfg(target_os = "linux")]
pub use linux::input_device_access;

/// No-op: Linux has no pane to open — the udev-rules guide is shown inline in
/// the Settings window instead.
#[cfg(not(target_os = "macos"))]
pub fn open_pane(_permission: Permission) {}

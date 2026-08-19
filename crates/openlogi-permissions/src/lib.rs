//! Privacy-permission status, and the deep links that take a user to the
//! system UI for fixing it.
//!
//! **Reading a status never prompts.** Prompting belongs to whichever process
//! owns the resource: the agent raises the Accessibility prompt because it owns
//! the event tap, and opens HID itself. A prompt raised from the wrong process
//! records the grant against the wrong code-signing identity (issue #214), so
//! this crate deliberately exposes only the non-prompting half plus
//! [`open_pane`]. That split is also why no general-purpose macOS permission
//! crate fits OpenLogi — they all assume one app asking for itself.
//!
//! ## macOS
//!
//! OpenLogi needs two real permissions: **Accessibility** (for the gesture /
//! button hook's event tap) and **Input Monitoring** (to open HID devices via
//! `IOHIDManager`). **Bluetooth** (CoreBluetooth) is surfaced for completeness;
//! OpenLogi reaches BLE mice via `IOHIDManager`, not CoreBluetooth, so it
//! usually reads [`PermissionStatus::Unknown`].
//!
//! Accessibility status is not read here: the agent owns the tap, so
//! `openlogi_hook::has_accessibility` is the source of truth and the app keeps
//! it live through its accessibility watcher. This crate covers the other two,
//! plus the System-Settings deep links for all of them.
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

/// No-op: Linux grants device access through udev rules, so there is no pane to
/// open. The install guide is shown inline in the Settings window instead.
#[cfg(not(target_os = "macos"))]
pub fn open_pane(_permission: Permission) {}

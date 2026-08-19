//! macOS Input Monitoring (TCC) status for the HID++ transport.
//!
//! `openlogi-hid` opens Logitech HID nodes through `IOHIDManager` (via
//! `async-hid`), which macOS gates behind the Input Monitoring privacy
//! permission — without it, every `IOHIDDeviceOpen` is silently denied and no
//! HID++ device ever appears, with no error surfaced beyond a debug log.
//!
//! Checking never prompts; [`request_access`] is the prompting call, and it
//! must run in this process (the agent), not the GUI — a TCC grant is scoped
//! to the code-signing identity that asks for it, and the agent (not the GUI)
//! is the one that actually opens HID devices.

use std::cfg_select;

#[cfg(target_os = "macos")]
mod macos {
    use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestAccess, IOHIDRequestType};

    pub(super) fn has_access() -> bool {
        matches!(
            IOHIDCheckAccess(IOHIDRequestType::ListenEvent),
            IOHIDAccessType::Granted
        )
    }

    pub(super) fn request_access() {
        // Unlike `AXIsProcessTrustedWithOptions`, `IOHIDRequestAccess` blocks
        // the calling thread until the user answers the consent dialog (or
        // returns immediately if the status is already determined) — callers
        // must run this off the async runtime.
        let _granted = IOHIDRequestAccess(IOHIDRequestType::ListenEvent);
    }
}

/// Whether this process currently holds Input Monitoring access.
///
/// Always `true` off macOS, where HID access has no privacy gate.
#[must_use]
pub fn has_access() -> bool {
    cfg_select! {
        target_os = "macos" => { macos::has_access() }
        _ => { true }
    }
}

/// Raise the macOS Input Monitoring consent dialog if not yet determined, so
/// this process (and not whichever process last called it) is the one listed
/// under System Settings → Privacy & Security → Input Monitoring.
///
/// Blocks the calling thread until the user responds — run it off the async
/// runtime (e.g. `tokio::task::spawn_blocking`). No-op off macOS.
pub fn request_access() {
    cfg_select! {
        target_os = "macos" => { macos::request_access(); }
        _ => {}
    }
}

//! macOS Input Monitoring (TCC) status for the HID++ transport, plus a
//! non-prompting Bluetooth authorization read for the agent's watcher.
//!
//! `openlogi-hid` opens Logitech HID nodes through `IOHIDManager` (via
//! `async-hid`), which macOS gates behind the Input Monitoring privacy
//! permission — without it, every `IOHIDDeviceOpen` is silently denied and no
//! HID++ device ever appears, with no error surfaced beyond a debug log.
//!
//! Checking never prompts; [`request_access`] is the Input Monitoring prompt,
//! and it must run in this process (the agent), not the GUI — a TCC grant is
//! scoped to the code-signing identity that asks for it. Bluetooth's prompt
//! lives in the agent (`permissions_macos`), not here.

use std::cfg_select;

#[cfg(target_os = "macos")]
mod macos {
    #![expect(
        unsafe_code,
        reason = "CoreBluetooth force-link + `+[CBManager authorization]` class-method send"
    )]

    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestAccess, IOHIDRequestType};

    // Force-link CoreBluetooth so `CBCentralManager` is registered for the
    // authorization lookup. Creating a manager (the prompting half) is not
    // this crate's job.
    #[link(name = "CoreBluetooth", kind = "framework")]
    unsafe extern "C" {}

    pub(super) fn has_access() -> bool {
        matches!(
            IOHIDCheckAccess(IOHIDRequestType::ListenEvent),
            IOHIDAccessType::Granted
        )
    }

    pub(super) fn request_access() -> bool {
        // Unlike `AXIsProcessTrustedWithOptions`, `IOHIDRequestAccess` blocks
        // the calling thread until the user answers the consent dialog (or
        // returns immediately if the status is already determined) — callers
        // must run this off the async runtime.
        //
        // The return value is the user's answer. `IOHIDCheckAccess` in this
        // same process can stay stale until relaunch, so callers that need to
        // restart after a grant must trust this bool, not [`has_access`].
        IOHIDRequestAccess(IOHIDRequestType::ListenEvent)
    }

    pub(super) fn has_bluetooth_access() -> bool {
        let Some(cls) = AnyClass::get(c"CBCentralManager") else {
            return false;
        };
        // SAFETY: `+[CBManager authorization]` is a documented class method
        // returning a `CBManagerAuthorization` NSInteger.
        let authorization: isize = unsafe { msg_send![cls, authorization] };
        authorization == 3
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
/// Returns whether the grant is now held. After a fresh Allow, prefer this
/// return over [`has_access`]: the check API can keep reporting the pre-grant
/// answer until the process restarts.
///
/// Blocks the calling thread until the user responds — run it off the async
/// runtime (e.g. `tokio::task::spawn_blocking`). Always `true` off macOS.
#[must_use]
pub fn request_access() -> bool {
    cfg_select! {
        target_os = "macos" => { macos::request_access() }
        _ => { true }
    }
}

/// Whether this process currently holds CoreBluetooth `allowedAlways`.
///
/// Query only — never creates a `CBCentralManager`. Always `false` off macOS.
#[must_use]
pub fn has_bluetooth_access() -> bool {
    cfg_select! {
        target_os = "macos" => { macos::has_bluetooth_access() }
        _ => { false }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn access_queries_do_not_prompt() {
        let _ = super::has_access();
        let _ = super::has_bluetooth_access();
    }
}

//! Secure Event Input on macOS: whether it is on, and which app macOS
//! attributes it to.
//!
//! While any process holds Secure Event Input — a focused password field,
//! Terminal's Secure Keyboard Entry, some password managers — the window
//! server delivers no `KeyDown` / `KeyUp` events to any event tap, this
//! crate's included. Mouse events and `FlagsChanged` still arrive. Both reads
//! run off the tap thread.
//!
//! `IsSecureEventInputEnabled` is a Carbon HIToolbox function, and no objc2
//! crate binds HIToolbox, so its `extern` declaration lives here, next to its
//! only caller.

use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFNumber, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

use super::foreground::foreground_app_from_running_application;
use crate::ForegroundApp;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// `Boolean IsSecureEventInputEnabled(void)` — `CarbonEvents.h`, macOS
    /// 10.3+. Takes nothing and reads session state, so it has no
    /// precondition to uphold.
    safe fn IsSecureEventInputEnabled() -> u8;
}

/// The session-dictionary key for the pid macOS attributes Secure Event Input
/// to. Not in the SDK headers; WindowServer publishes it in both the
/// `CGSessionCopyCurrentDictionary` dictionary and `IOConsoleUsers`, and only
/// while Secure Event Input is on.
const SECURE_INPUT_PID_KEY: &str = "kCGSSessionSecureInputPID";

/// Whether any process holds Secure Event Input in this login session.
pub(crate) fn secure_input_enabled() -> bool {
    IsSecureEventInputEnabled() != 0
}

/// The app macOS attributes Secure Event Input to, if any.
///
/// The attribution is WindowServer's, not the holder's own claim: measured on
/// macOS 26, a background process that turns it on is attributed to the
/// frontmost app instead. `None` when it is off, when the session dictionary
/// carries no pid, or when the pid is not a running app.
pub(crate) fn secure_input_app() -> Option<ForegroundApp> {
    let pid = secure_input_pid()?;
    objc2::rc::autoreleasepool(|pool| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        foreground_app_from_running_application(&app, pool)
    })
}

fn secure_input_pid() -> Option<i32> {
    let session = CGSessionCopyCurrentDictionary()?;
    // SAFETY: `CGSession.h` documents the dictionary as keyed by CFStrings
    // with CoreFoundation values; each value is downcast before use.
    let session = unsafe { session.cast_unchecked::<CFString, CFType>() };
    let pid = session
        .get(&CFString::from_static_str(SECURE_INPUT_PID_KEY))?
        .downcast_ref::<CFNumber>()?
        .as_i32()?;
    (pid > 0).then_some(pid)
}

use std::ffi::{c_int, c_void};

use core_foundation::base::TCFType;
use core_foundation::string::CFString;

use super::app_services_symbol;

/// Show all windows across spaces (Mission Control).
pub(super) fn mission_control() {
    send("com.apple.expose.awake");
}

/// Show the front app's windows (App Exposé).
pub(super) fn app_expose() {
    send("com.apple.expose.front.awake");
}

/// Move all windows aside to reveal the desktop.
pub(super) fn show_desktop() {
    send("com.apple.showdesktop.awake");
}

/// Toggle Launchpad. A no-op on macOS 26, which removed Launchpad.
pub(super) fn launchpad() {
    send("com.apple.launchpad.toggle");
}

/// Post `notification` to the Dock. Logs and returns on any failure.
fn send(notification: &str) {
    let Some(core_dock_send) = core_dock_send_notification() else {
        tracing::warn!(notification, "CoreDockSendNotification unavailable");
        return;
    };
    let name = CFString::new(notification);
    // SAFETY: resolved AppServices symbol called with its documented
    // signature; `name` is a live CFString for the call's duration.
    let err = unsafe { core_dock_send(name.as_concrete_TypeRef().cast(), 0) };
    if err != 0 {
        tracing::warn!(notification, err, "CoreDockSendNotification failed");
    }
}

type CoreDockSendNotificationFn = unsafe extern "C" fn(*const c_void, c_int) -> c_int;

/// Resolve `CoreDockSendNotification` from `ApplicationServices`, caching
/// the `dlopen` handle for the process lifetime. `None` if unavailable.
fn core_dock_send_notification() -> Option<CoreDockSendNotificationFn> {
    let sym = app_services_symbol(c"CoreDockSendNotification")?;
    // SAFETY: the symbol, when present, has the documented signature.
    Some(unsafe { std::mem::transmute::<*mut c_void, CoreDockSendNotificationFn>(sym) })
}

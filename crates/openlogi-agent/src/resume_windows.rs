//! Native Windows suspend/resume notifications for the agent core.
//!
//! Mirrors the macOS workspace-wake observer in [`crate::tray`]: volatile
//! HID++ settings (DPI, SmartShift, wheel mode, lighting) live in device RAM
//! and clear when devices power-cycle across a system sleep, but the first
//! post-wake inventory snapshot can look identical to the last pre-sleep one,
//! so no per-device transition re-applies them (#393, #527). The inventory
//! watcher's wall-clock-gap heuristic only catches sleeps longer than a
//! minute; the native notification covers the rest.
//!
//! `RegisterSuspendResumeNotification` with `DEVICE_NOTIFY_CALLBACK` rather
//! than a `WM_POWERBROADCAST` window: it needs no message pump, and it fires
//! regardless of the tray preference — the tray window only exists when
//! `show_in_menu_bar` is on.

#![expect(
    unsafe_code,
    reason = "raw win32: RegisterSuspendResumeNotification + its callback — localized here"
)]

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, RegisterSuspendResumeNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
};

/// Register for suspend/resume notifications for the process lifetime; the
/// system invokes [`on_power_event`] and each resume sets `pending`. Failure
/// is logged, never fatal — the polling-gap heuristic still covers long
/// sleeps.
pub fn register(pending: Arc<AtomicBool>) {
    // Leaked on purpose: the subscription is never unregistered, so the
    // callback may read the flag until process exit.
    let context = Arc::into_raw(pending);
    // Also leaked: with `DEVICE_NOTIFY_CALLBACK` the recipient *is* this
    // parameter block, and the subscription may hold the pointer for its
    // whole lifetime — a stack-local here would dangle once this returns.
    let params = Box::into_raw(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_power_event),
        Context: context.cast_mut().cast::<c_void>(),
    }));
    // SAFETY: `params` and the Arc behind its context are both leaked for the
    // process lifetime, matching the never-unregistered subscription, and the
    // callback matches `PDEVICE_NOTIFY_CALLBACK_ROUTINE`.
    let handle = unsafe {
        RegisterSuspendResumeNotification(params.cast::<c_void>(), DEVICE_NOTIFY_CALLBACK)
    };
    if handle == 0 {
        warn!("suspend/resume registration failed — only the polling-gap heuristic detects wakes");
    } else {
        info!("suspend/resume notifications registered");
    }
}

/// Whether a `PBT_*` power event means the system just resumed.
/// `PBT_APMRESUMEAUTOMATIC` fires on every wake, `PBT_APMRESUMESUSPEND`
/// additionally once user input confirms it — both can arrive for one wake,
/// and the flag coalesces them.
fn is_resume_event(event: u32) -> bool {
    matches!(event, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND)
}

/// Invoked by the system on an arbitrary thread; only touches the atomic.
unsafe extern "system" fn on_power_event(
    context: *const c_void,
    event: u32,
    _setting: *const c_void,
) -> u32 {
    if is_resume_event(event) {
        // SAFETY: `context` is the `Arc<AtomicBool>` this module leaked at
        // registration, alive for the process lifetime.
        let pending = unsafe { &*context.cast::<AtomicBool>() };
        pending.store(true, Ordering::Relaxed);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::PBT_APMSUSPEND;

    #[test]
    fn resume_events_set_the_flag_and_a_suspend_does_not() {
        let pending = AtomicBool::new(false);
        let context = (&raw const pending).cast::<c_void>();
        for event in [PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND] {
            // SAFETY: `context` points at the flag above, live for the whole
            // test; the callback runs synchronously here.
            unsafe { on_power_event(context, event, std::ptr::null()) };
            assert!(pending.swap(false, Ordering::Relaxed), "event {event}");
        }
        // SAFETY: same live context; a suspend must not request a re-apply.
        unsafe { on_power_event(context, PBT_APMSUSPEND, std::ptr::null()) };
        assert!(!pending.load(Ordering::Relaxed));
    }
}

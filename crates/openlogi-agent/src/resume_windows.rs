//! Native Windows suspend/resume notifications for the agent core.
//!
//! Mirrors the macOS workspace-wake observer in [`crate::tray`]: volatile
//! HID++ settings (DPI, SmartShift, wheel mode, lighting) live in device RAM
//! and clear when devices power-cycle across a system sleep, but the first
//! post-wake inventory snapshot can look identical to the last pre-sleep one,
//! so no per-device transition re-applies them (#393, #527). The inventory
//! watcher's clock-gap heuristic only catches sleeps longer than a
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

use openlogi_agent_core::watchers::inventory::ResumeSignal;
use tracing::{info, warn};
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, RegisterSuspendResumeNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
};

/// Register for suspend/resume notifications for the process lifetime; the
/// system invokes [`on_power_event`] and each resume notifies `signal`. Failure
/// is logged, never fatal — the clock-gap heuristic still covers long
/// sleeps.
pub fn register(signal: ResumeSignal) {
    // Retained for the process lifetime on successful registration: the
    // callback may notify the signal until process exit.
    let context = Box::into_raw(Box::new(signal));
    // Likewise, with `DEVICE_NOTIFY_CALLBACK` the recipient *is* this
    // parameter block, so a successful subscription may hold its pointer for
    // the whole lifetime — a stack-local here would dangle once this returns.
    let params = Box::into_raw(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_power_event),
        Context: context.cast::<c_void>(),
    }));
    // SAFETY: `params` and the `ResumeSignal` behind its context remain valid
    // for the call, and the callback matches
    // `PDEVICE_NOTIFY_CALLBACK_ROUTINE`. A successful subscription keeps both
    // allocations for the process lifetime below.
    let handle = unsafe {
        RegisterSuspendResumeNotification(params.cast::<c_void>(), DEVICE_NOTIFY_CALLBACK)
    };
    if handle == 0 {
        // SAFETY: registration failed, so Windows retained neither pointer and
        // cannot invoke the callback with `context` after this call.
        unsafe {
            drop(Box::from_raw(params));
            drop(Box::from_raw(context));
        }
        warn!("suspend/resume registration failed — only the clock-gap heuristic detects wakes");
    } else {
        info!("suspend/resume notifications registered");
    }
}

/// Whether a `PBT_*` power event means the system just resumed.
/// `PBT_APMRESUMEAUTOMATIC` fires on every wake, `PBT_APMRESUMESUSPEND`
/// additionally once user input confirms it — both can arrive for one wake,
/// and the bounded resume channel coalesces them.
fn is_resume_event(event: u32) -> bool {
    matches!(event, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND)
}

/// Invoked by the system on an arbitrary thread; only attempts a non-blocking
/// send to the bounded resume channel.
unsafe extern "system" fn on_power_event(
    context: *const c_void,
    event: u32,
    _setting: *const c_void,
) -> u32 {
    if is_resume_event(event) {
        // SAFETY: `context` is the `ResumeSignal` this module leaked at
        // registration, alive for the process lifetime.
        let signal = unsafe { &*context.cast::<ResumeSignal>() };
        signal.notify();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_agent_core::watchers::inventory::resume_channel;
    use windows_sys::Win32::UI::WindowsAndMessaging::PBT_APMSUSPEND;

    #[tokio::test]
    async fn resume_events_notify_and_a_suspend_does_not() {
        let (signal, mut events) = resume_channel();
        let context = (&raw const signal).cast::<c_void>();
        // SAFETY: `context` points at the signal above, live for this test;
        // the callback executes synchronously.
        unsafe { on_power_event(context, PBT_APMSUSPEND, std::ptr::null()) };
        tokio::time::timeout(std::time::Duration::from_millis(10), events.recv())
            .await
            .expect_err("a suspend edge does not notify inventory");

        // SAFETY: `context` points at the signal above, live for this test;
        // the callback executes synchronously.
        unsafe { on_power_event(context, PBT_APMRESUMEAUTOMATIC, std::ptr::null()) };
        tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("resume callback notifies the inventory signal")
            .expect("the signal producer remains alive");
        assert!(is_resume_event(PBT_APMRESUMESUSPEND));
    }
}

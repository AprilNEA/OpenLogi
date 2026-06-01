//! macOS `CGEventTap` implementation of the OS-level mouse hook.

use std::sync::{Arc, mpsc};
use std::thread;

use core_foundation::runloop::{
    CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult, EventField,
};
use tracing::{debug, error, warn};

use crate::{ButtonId, EventDisposition, HookError, MouseEvent};

/// Everything `Hook` needs to control the background thread.
pub(crate) struct HookInner {
    thread: thread::JoinHandle<()>,
    run_loop: CFRunLoop,
}

// SAFETY: CFRunLoop is a Core Foundation ref-counted object. The CF
// documentation states that CFRunLoop objects can be passed between
// threads; only CFRunLoopRun must be called on the owning thread.
unsafe impl Send for HookInner {}

// Raw FFI for `AXIsProcessTrustedWithOptions` from the Accessibility
// framework. Passing `NULL` queries trust state without prompting; passing
// a dictionary with `kAXTrustedCheckOptionPrompt = true` raises the system
// permission dialog and registers the process in the Accessibility list.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
}

/// Check whether this process has been granted Accessibility access.
///
/// `AXIsProcessTrustedWithOptions(NULL)` is the canonical API, but it can
/// return `false` for locally-built / ad-hoc-signed binaries even after the
/// user toggles the permission on in System Settings, because the TCC entry
/// that was created by a previous (differently-signed) build does not match
/// the current binary's code-signing identity.
///
/// When the API returns `false` we fall back to reading the user TCC database
/// directly. `auth_value = 2` means "allowed"; any other value (0 = denied,
/// 1 = limited) keeps `false`. This fallback is intentionally best-effort:
/// if `sqlite3` is unavailable or the DB path changes, we silently return the
/// API result unchanged.
pub(crate) fn has_accessibility() -> bool {
    // SAFETY: NULL is documented as a valid argument; it queries the current
    // trust state without raising a permission dialog.
    if unsafe { AXIsProcessTrustedWithOptions(std::ptr::null()) } {
        return true;
    }
    // TCC fallback for locally-built / ad-hoc-signed binaries.
    if tcc_accessibility_allowed() {
        debug!(
            "AXIsProcessTrustedWithOptions returned false but TCC db shows \
             auth_value=2 — treating as granted (locally-signed build)"
        );
        return true;
    }
    false
}

/// Return `true` when the user TCC database records `auth_value = 2`
/// (allowed) for `kTCCServiceAccessibility` and this app's bundle ID.
fn tcc_accessibility_allowed() -> bool {
    const BUNDLE_ID: &str = "org.openlogi.openlogi";
    const QUERY: &str =
        "SELECT auth_value FROM access \
         WHERE service='kTCCServiceAccessibility' AND client=?1 LIMIT 1";

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return false,
    };
    let db = format!(
        "{home}/Library/Application Support/com.apple.TCC/TCC.db"
    );
    let out = std::process::Command::new("sqlite3")
        .args([&db, &format!("{QUERY}"), BUNDLE_ID])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim() == "2"
        }
        Ok(o) => {
            debug!(
                status = %o.status,
                "sqlite3 TCC query exited non-zero; skipping accessibility fallback"
            );
            false
        }
        Err(e) => {
            debug!(error = %e, "sqlite3 not available; skipping accessibility fallback");
            false
        }
    }
}

/// Raise the Accessibility prompt + register the process. See
/// [`super::Hook::prompt_accessibility`].
pub(crate) fn prompt_accessibility() {
    use core_foundation::base::TCFType as _;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    // SAFETY: `kAXTrustedCheckOptionPrompt` is a framework-provided
    // `CFStringRef` constant; wrapping under the get rule borrows it
    // without taking ownership.
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options =
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
    // SAFETY: `options` is a valid `CFDictionaryRef` for the lifetime of
    // the call; the function reads it and (if untrusted) shows the dialog.
    // The returned trust state is observed separately via the watcher.
    let _trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast()) };
}

/// Read the frontmost application's bundle identifier via NSWorkspace.
/// Pure FFI — returns `None` when no app is frontmost or the identifier
/// is missing / non-UTF8.
///
/// Wrapped in a per-call `NSAutoreleasePool`. Without it, every call on
/// a non-main thread (the watcher loop) leaks the workspace, app, and
/// bundle-id objects — at one call per second that's hundreds of MB
/// after a full workday.
pub(crate) fn frontmost_bundle_id() -> Option<String> {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    // SAFETY: NSWorkspace is part of AppKit, available on every supported
    // macOS (≥13.0). Each `msg_send!` returns either `nil` (handled below)
    // or an autoreleased Objective-C object. The surrounding
    // `NSAutoreleasePool` drains those temporaries when this function
    // returns; the Rust `String` we hand back is a copy that outlives
    // the pool.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace == nil {
            let _: () = msg_send![pool, drain];
            return None;
        }
        let app: id = msg_send![workspace, frontmostApplication];
        if app == nil {
            let _: () = msg_send![pool, drain];
            return None;
        }
        let bundle_id: id = msg_send![app, bundleIdentifier];
        if bundle_id == nil {
            let _: () = msg_send![pool, drain];
            return None;
        }
        let ptr: *const std::os::raw::c_char = NSString::UTF8String(bundle_id);
        let result = if ptr.is_null() {
            None
        } else {
            std::ffi::CStr::from_ptr(ptr)
                .to_str()
                .ok()
                .map(str::to_owned)
        };
        let _: () = msg_send![pool, drain];
        result
    }
}

/// Translate a raw OS button number to a [`ButtonId`].
///
/// Logi's convention: button 0 = left, 1 = right, 2 = middle, 3 = back,
/// 4 = forward. Numbers ≥5 don't map to a `ButtonId` we track.
fn button_number_to_id(n: i64) -> Option<ButtonId> {
    match n {
        0 => Some(ButtonId::LeftClick),
        1 => Some(ButtonId::RightClick),
        2 => Some(ButtonId::MiddleClick),
        3 => Some(ButtonId::Back),
        4 => Some(ButtonId::Forward),
        _ => None,
    }
}

/// Convert a `CGEvent` to our [`MouseEvent`] vocabulary. Returns `None`
/// for event types we don't translate (e.g. move events, unknown buttons).
fn translate(etype: CGEventType, event: &CGEvent) -> Option<MouseEvent> {
    match etype {
        CGEventType::LeftMouseDown => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: true,
        }),
        CGEventType::LeftMouseUp => Some(MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: false,
        }),
        CGEventType::RightMouseDown => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: true,
        }),
        CGEventType::RightMouseUp => Some(MouseEvent::Button {
            id: ButtonId::RightClick,
            pressed: false,
        }),
        CGEventType::OtherMouseDown => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button { id, pressed: true })
        }
        CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            button_number_to_id(n).map(|id| MouseEvent::Button { id, pressed: false })
        }
        CGEventType::ScrollWheel => {
            // axis 1 = vertical scroll; axis 2 = horizontal scroll.
            let dy = event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
            let dx = event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scroll deltas are small fractional values that fit comfortably in f32"
            )]
            Some(MouseEvent::Scroll {
                delta_x: dx as f32,
                delta_y: dy as f32,
            })
        }
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
            error!(
                "CGEventTap disabled by OS (type={etype:?}); \
                 hook will stop receiving events until re-enabled"
            );
            None
        }
        _ => None,
    }
}

/// Create the event tap and run loop on a dedicated thread.
pub(crate) fn start(
    cb: impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static,
) -> Result<HookInner, HookError> {
    if !has_accessibility() {
        return Err(HookError::AccessibilityDenied);
    }

    // Wrap in Arc so the closure handed to CGEventTap::new captures it by
    // clone rather than by move — avoids a second Box allocation.
    let cb: Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync> = Arc::new(cb);

    let (rl_tx, rl_rx) = mpsc::channel::<CFRunLoop>();

    let thread = thread::Builder::new()
        .name("openlogi-hook".into())
        .spawn(move || thread_main(cb, rl_tx))
        .map_err(|e| HookError::MacOsTap(e.to_string()))?;

    // Block until the background thread confirms the run loop is live, or
    // reports failure by dropping its sender.
    let run_loop = rl_rx.recv().map_err(|_| {
        HookError::MacOsTap(
            "background thread exited before the run loop started; \
             CGEventTapCreate likely returned null"
                .into(),
        )
    })?;

    Ok(HookInner { thread, run_loop })
}

/// Body of the background hook thread.
#[allow(
    clippy::needless_pass_by_value,
    reason = "rl_tx must be owned: dropping it signals the parent's recv() to return Err on failure paths"
)]
fn thread_main(
    cb: Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync>,
    rl_tx: mpsc::Sender<CFRunLoop>,
) {
    let event_types = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
    ];

    let tap_result = CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        event_types,
        move |_proxy: CGEventTapProxy, etype: CGEventType, event: &CGEvent| {
            let Some(mouse_event) = translate(etype, event) else {
                return CallbackResult::Keep;
            };
            match cb(mouse_event) {
                EventDisposition::PassThrough => CallbackResult::Keep,
                EventDisposition::Suppress => CallbackResult::Drop,
            }
        },
    );

    let Ok(tap) = tap_result else {
        error!("CGEventTapCreate returned null — Accessibility may have been revoked");
        // Dropping rl_tx causes rl_rx.recv() on the parent to return Err,
        // which we surface as MacOsTap.
        return;
    };

    let Ok(loop_source) = tap.mach_port().create_runloop_source(0) else {
        error!("CFRunLoopSourceCreate failed for event tap");
        return;
    };

    let run_loop = CFRunLoop::get_current();

    // SAFETY: kCFRunLoopCommonModes is a static CF string constant that
    // lives for the duration of the process.
    unsafe {
        run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }
    tap.enable();

    if rl_tx.send(run_loop).is_err() {
        debug!("hook parent dropped before run loop was ready; stopping");
        return;
    }

    // Service the tap in short slices instead of an unbounded
    // `run_current()`. Between slices we re-check Accessibility: an active
    // tap at the HID location that outlives its permission wedges the
    // *entire* system input stream — mouse and keyboard alike — until
    // reboot. If the user revokes access while we're live, tear the tap
    // down right here, on the tap's own thread, so input is restored even
    // when the UI thread is already stuck. `stop()` (normal shutdown)
    // returns `Stopped` and also breaks the loop.
    loop {
        match CFRunLoop::run_in_mode(
            // SAFETY: framework-provided static CFStringRef, 'static.
            unsafe { kCFRunLoopDefaultMode },
            std::time::Duration::from_millis(500),
            false,
        ) {
            CFRunLoopRunResult::Stopped | CFRunLoopRunResult::Finished => break,
            CFRunLoopRunResult::TimedOut | CFRunLoopRunResult::HandledSource => {}
        }
        if !has_accessibility() {
            warn!(
                "Accessibility revoked while the event tap was live — \
                 disabling the tap to avoid wedging system input"
            );
            break;
        }
    }

    // Detach the tap from the event stream synchronously before unwinding,
    // so input recovers immediately rather than whenever CF happens to
    // release the port.
    disable_tap(&tap);
}

/// Disable an active event tap now. core-graphics only exposes the enable
/// side of `CGEventTapEnable`, so we bind the disable side ourselves.
fn disable_tap(tap: &CGEventTap) {
    use core_foundation::base::TCFType as _;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapEnable(tap: core_foundation::mach_port::CFMachPortRef, enable: bool);
    }

    // SAFETY: `tap.mach_port()` is a live `CFMachPort` for the duration of
    // the call; `CGEventTapEnable(.., false)` is idempotent and merely
    // detaches the tap from the system event stream.
    unsafe { CGEventTapEnable(tap.mach_port().as_concrete_TypeRef(), false) };
}

/// Signal the run loop to stop and join the background thread.
pub(crate) fn stop(inner: HookInner) {
    inner.run_loop.stop();
    if let Err(e) = inner.thread.join() {
        error!("hook thread panicked on shutdown: {e:?}");
    }
}

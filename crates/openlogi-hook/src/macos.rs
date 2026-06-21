//! macOS `CGEventTap` implementation of the OS-level mouse hook.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use core_foundation::runloop::{
    CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use core_graphics::event::{
    CGEvent, CGEventField, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult, EventField,
};
use tracing::{debug, error, warn};

use crate::{ButtonId, EventDisposition, HookError, MouseEvent};

/// Everything `Hook` needs to control the background thread.
pub(crate) struct HookInner {
    thread: thread::JoinHandle<()>,
    run_loop: CFRunLoop,
    /// Termination flag, re-checked at the top of every run-loop slice.
    /// `run_loop.stop()` only interrupts the loop while it is *inside* a
    /// `run_in_mode` slice; a stop landing in the gap between slices is
    /// dropped, so this flag — not the CF stop alone — is the reliable
    /// shutdown signal that guarantees the thread can never hang forever.
    stop: Arc<AtomicBool>,
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
pub(crate) fn has_accessibility() -> bool {
    // SAFETY: NULL is documented as a valid argument; it queries the current
    // trust state without raising a permission dialog.
    unsafe { AXIsProcessTrustedWithOptions(std::ptr::null()) }
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

/// Read the frontmost application's bundle identifier via `NSWorkspace`.
/// Returns `None` when no app is frontmost or the identifier is missing.
///
/// `NSWorkspace` is `AnyThread`, so this is sound on the watcher thread. The
/// reads return owned `Retained` values (no leak by construction), but the
/// framework still autoreleases internal temporaries and `to_str` borrows its
/// UTF-8 view from the pool — so an explicit `autoreleasepool` is required off
/// the main thread, where no run loop drains one. (Without it the old raw path
/// leaked the workspace/app/bundle-id objects: hundreds of MB across a workday.)
pub(crate) fn frontmost_bundle_id() -> Option<String> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;

    autoreleasepool(|pool| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let bundle_id = app.bundleIdentifier()?;
        // SAFETY: `to_str` yields a UTF-8 view valid for `pool`'s lifetime; we
        // copy it into an owned `String` before the pool (and `bundle_id`) drop,
        // so the borrow never escapes.
        Some(unsafe { bundle_id.to_str(pool) }.to_owned())
    })
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
    // Skip events OpenLogi itself synthesised ([`Action::execute`] stamps them),
    // so a remapped click we posted doesn't re-enter the hook as real input and,
    // for a gesture button, get misread as a fresh hold. Only button events are
    // ever synthesised (`Action::execute` posts buttons, never moves/scroll), so
    // gate the field read on button types — keeping the FFI call off the
    // high-rate pointer-move stream.
    let is_button = matches!(
        etype,
        CGEventType::LeftMouseDown
            | CGEventType::LeftMouseUp
            | CGEventType::RightMouseDown
            | CGEventType::RightMouseUp
            | CGEventType::OtherMouseDown
            | CGEventType::OtherMouseUp
    );
    if is_button
        && event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
            == openlogi_inject::SYNTHETIC_EVENT_USER_DATA
    {
        return None;
    }
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
            // Continuous events come from a trackpad / Magic Mouse gesture; a
            // notched mouse wheel is discrete. The consumer uses this to invert
            // the wheel without disturbing native trackpad scrolling (#126).
            let is_continuous =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scroll deltas are small fractional values that fit comfortably in f32"
            )]
            Some(MouseEvent::Scroll {
                delta_x: dx as f32,
                delta_y: dy as f32,
                is_continuous,
            })
        }
        // Pointer movement feeds gesture-button swipe detection. While a button
        // is physically held the OS reports *Dragged rather than MouseMoved, so
        // a gesture button's hold-and-swipe arrives here as OtherMouseDragged.
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "per-event pointer deltas are small integers, far within i32"
            )]
            Some(MouseEvent::Moved {
                delta_x: dx as i32,
                delta_y: dy as i32,
            })
        }
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
            // The run-loop slice re-enables the tap (see `thread_main`); surface
            // the interruption so the runtime cancels any in-progress hold — a
            // button-up dropped during the gap must not later fire a phantom
            // swipe off ordinary cursor motion. Logged at debug, not warn:
            // TapDisabledByUserInput fires during ordinary heavy input bursts and
            // self-heals next slice, so it isn't worth a warning each time.
            debug!("CGEventTap disabled by OS (type={etype:?}); re-enabling, cancelling any hold");
            Some(MouseEvent::CaptureInterrupted)
        }
        _ => None,
    }
}

/// Rewrite a live `ScrollWheel` event so its deltas become `(new_dx, new_dy)`
/// in the line units the tap reports. macOS hands modern apps a pixel-precise
/// delta and a fixed-point delta *alongside* the integer line delta, and an app
/// with smooth scrolling reads those, not the line field — so flipping only the
/// line delta would leave such apps scrolling the original way. Each axis scales
/// all three companion fields by the same factor (`-1` for the inversion in
/// #126, which is exact), keeping the smooth-scroll path consistent with the
/// notched one.
fn rewrite_scroll(event: &CGEvent, horizontal: f64, vertical: f64) {
    // Vertical = axis 1, horizontal = axis 2 (mirrors `translate`).
    rewrite_scroll_axis(
        event,
        vertical,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
    );
    rewrite_scroll_axis(
        event,
        horizontal,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
    );
}

/// Rewrite one scroll axis: set its line delta to `new_line` and scale the
/// fixed-point + pixel companions by `new_line / old_line` so they agree. A
/// zero original delta means no scroll on this axis, so the companions are left
/// untouched (and the division avoided).
fn rewrite_scroll_axis(
    event: &CGEvent,
    new_line: f64,
    line_field: CGEventField,
    fixed_field: CGEventField,
    point_field: CGEventField,
) {
    let old_line = event.get_double_value_field(line_field);
    event.set_double_value_field(line_field, new_line);
    if old_line != 0.0 {
        let scale = new_line / old_line;
        let fixed = event.get_double_value_field(fixed_field);
        event.set_double_value_field(fixed_field, fixed * scale);
        let point = event.get_double_value_field(point_field);
        event.set_double_value_field(point_field, point * scale);
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

    let stop = Arc::new(AtomicBool::new(false));
    let (rl_tx, rl_rx) = mpsc::channel::<CFRunLoop>();

    let thread = {
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("openlogi-hook".into())
            .spawn(move || thread_main(cb, rl_tx, stop))
            .map_err(|e| HookError::MacOsTap(e.to_string()))?
    };

    // Block until the background thread confirms the run loop is live, or
    // reports failure by dropping its sender.
    let run_loop = rl_rx.recv().map_err(|_| {
        HookError::MacOsTap(
            "background thread exited before the run loop started; \
             CGEventTapCreate likely returned null"
                .into(),
        )
    })?;

    Ok(HookInner {
        thread,
        run_loop,
        stop,
    })
}

/// Body of the background hook thread.
#[allow(
    clippy::needless_pass_by_value,
    reason = "rl_tx must be owned: dropping it signals the parent's recv() to return Err on failure paths"
)]
fn thread_main(
    cb: Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync>,
    rl_tx: mpsc::Sender<CFRunLoop>,
    stop: Arc<AtomicBool>,
) {
    let event_types = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
        // Pointer movement, for gesture-button hold+swipe. A held button makes
        // the OS emit *Dragged rather than MouseMoved, so all four are needed.
        // The callback stays lock-light (see `hook_runtime`) so this high-rate
        // stream can't stall the tap.
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
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
                EventDisposition::ReplaceScroll { delta_x, delta_y } => {
                    rewrite_scroll(event, f64::from(delta_x), f64::from(delta_y));
                    CallbackResult::Keep
                }
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
    // when the UI thread is already stuck.
    //
    // `stop()` requests shutdown two ways: it sets `stop` and calls
    // `run_loop.stop()`. The CF stop returns `Stopped` and breaks promptly
    // while a slice is running, but is a no-op if it lands in the gap
    // between slices (CFRunLoopStop only acts on a running loop). The `stop`
    // flag, checked at the top of every slice, is the reliable signal: in
    // that race the thread notices one 500 ms slice later instead of joining
    // forever.
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
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
        // Recover from an OS-initiated disable (TapDisabledByTimeout/UserInput):
        // re-enabling is idempotent while the tap is already live, so this brings
        // a disabled tap back within one slice instead of the hook going
        // permanently deaf. Only reached while Accessibility is still granted.
        tap.enable();
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
    // Set the flag *before* waking the loop: if `run_loop.stop()` lands in
    // the gap between slices and is dropped, the thread still observes the
    // flag at the next slice top. Relaxed suffices — the flag carries no
    // other data, and `thread.join()` below is the final synchronisation.
    inner.stop.store(true, Ordering::Relaxed);
    inner.run_loop.stop();
    if let Err(e) = inner.thread.join() {
        error!("hook thread panicked on shutdown: {e:?}");
    }
}

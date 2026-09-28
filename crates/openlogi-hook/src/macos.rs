//! macOS `CGEventTap` implementation of the OS-level mouse hook.
#![expect(
    unsafe_code,
    reason = "the event tap uses Core Graphics / Core Foundation C APIs, and workspace observation uses typed Objective-C notification APIs"
)]

mod foreground;
pub(crate) mod pointer;
mod sender;
mod translate;
mod watchdog;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use core_foundation::runloop::{
    CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2_app_kit::NSWorkspace;
use objc2_application_services::{AXIsProcessTrusted, AXIsProcessTrustedWithOptions};
use tracing::{debug, error, warn};

use crate::{
    CursorPosition, EventDisposition, EventTapInfo, ForegroundApp, HookBackend, HookError,
    HookEvent, TapLocation,
};
pub use foreground::ForegroundApplicationObserver;
use foreground::observe_frontmost_application;
pub(crate) use foreground::{frontmost_safari_pid, watch_frontmost_application_activations};
use translate::{translate, translate_key};
use watchdog::{
    CallbackActivity, LifecycleDecision, LifecycleExitReason, LifecycleObservation,
    LifecycleWatchdog, RearmBudget, TapPhase, WatchdogSignals, stuck_callback,
};

/// Everything `Hook` needs to control the background thread.
pub(crate) struct HookInner {
    thread: thread::JoinHandle<()>,
    lifecycle_watchdog: thread::JoinHandle<()>,
    run_loop: CFRunLoop,
    /// Lifecycle signals re-checked at the top of every run-loop slice.
    /// `run_loop.stop()` only interrupts the loop while it is *inside* a
    /// `run_in_mode` slice; a stop landing in the gap between slices is
    /// dropped, so the stop latch — not the CF stop alone — is the reliable
    /// shutdown signal. The independent lifecycle watchdog keeps observing
    /// that latch until the tap thread proves the tap is gone.
    signals: Arc<WatchdogSignals>,
}

// SAFETY: CFRunLoop is a Core Foundation ref-counted object. The CF
// documentation states that CFRunLoop objects can be passed between
// threads; only CFRunLoopRun must be called on the owning thread.
unsafe impl Send for HookInner {}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // `core-graphics` exposes only the enable-true operation, and does not
    // expose the state read used to budget re-arms.
    fn CGEventTapEnable(tap: core_foundation::mach_port::CFMachPortRef, enable: bool);
    fn CGEventTapIsEnabled(tap: core_foundation::mach_port::CFMachPortRef) -> bool;
}

/// Can this process create an *active* (event-filtering) tap right now?
///
/// The probe mirrors the real tap's location, placement and options — that is
/// the capability being tested — but subscribes to `kCGEventNull`, an event
/// type nothing ever posts, so it cannot gate a single real event during the
/// microseconds it exists. Dropping it invalidates the port.
fn can_filter_events() -> bool {
    CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::Null],
        |_proxy: CGEventTapProxy, _etype: CGEventType, _event: &CGEvent| CallbackResult::Keep,
    )
    .is_ok()
}

const CALLBACK_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(20);
const LIFECYCLE_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FREEZE_HAZARD_EXIT_CODE: i32 = 78;

/// Event types the HID tap observes. Pointer *Dragged variants are required
/// because a held button makes the OS emit those instead of `MouseMoved`.
/// The macOS backend: a `CGEventTap` serviced by a private run-loop thread.
pub(crate) struct Backend;

impl HookBackend for Backend {
    type Running = HookInner;

    /// Create the event tap and run loop on a dedicated thread.
    fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<HookInner, HookError> {
        if !Self::has_accessibility() {
            return Err(HookError::AccessibilityDenied);
        }

        // Seed the press-time snapshot before the tap can receive input. Later
        // NSWorkspace activation notifications refresh it off the tap thread.
        let _ = Self::frontmost_app();

        // Wrap in Arc so the closure handed to CGEventTap::new captures it by
        // clone rather than by move — avoids a second Box allocation.
        let cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync> = Arc::new(cb);

        let signals = Arc::new(WatchdogSignals::default());
        let lifecycle_watchdog = spawn_lifecycle_watchdog(Arc::clone(&signals))?;
        let (rl_tx, rl_rx) = mpsc::channel::<CFRunLoop>();

        let thread = {
            let thread_signals = Arc::clone(&signals);
            match thread::Builder::new()
                .name("openlogi-hook".into())
                .spawn(move || thread_main(cb, rl_tx, thread_signals))
            {
                Ok(thread) => thread,
                Err(error) => {
                    signals.set_phase(TapPhase::ThreadExited);
                    lifecycle_watchdog.thread().unpark();
                    let _ = lifecycle_watchdog.join();
                    return Err(HookError::MacOsTap(error.to_string()));
                }
            }
        };

        // Block until the background thread confirms the run loop is live, or
        // reports failure by dropping its sender.
        let Ok(run_loop) = rl_rx.recv() else {
            let error = HookError::MacOsTap(
                "background thread exited before the run loop started; \
                 CGEventTapCreate likely returned null"
                    .into(),
            );
            if let Err(panic) = thread.join() {
                error!(?panic, "hook thread panicked during startup");
            }
            lifecycle_watchdog.thread().unpark();
            if let Err(panic) = lifecycle_watchdog.join() {
                error!(?panic, "hook lifecycle watchdog panicked during startup");
            }
            return Err(error);
        };

        Ok(HookInner {
            thread,
            lifecycle_watchdog,
            run_loop,
            signals,
        })
    }

    /// Signal the run loop to stop and join the background thread.
    fn stop(inner: HookInner) {
        // Latch stop before waking either thread. The lifecycle watchdog stays
        // armed across the blocking join and accepts only `ThreadExited` as proof
        // that explicit shutdown completed.
        inner.signals.request_stop();
        inner.lifecycle_watchdog.thread().unpark();
        inner.run_loop.stop();
        if let Err(e) = inner.thread.join() {
            error!("hook thread panicked on shutdown: {e:?}");
        }
        inner.lifecycle_watchdog.thread().unpark();
        if let Err(e) = inner.lifecycle_watchdog.join() {
            error!("hook lifecycle watchdog panicked on shutdown: {e:?}");
        }
    }

    /// Check whether this process can still install the hook's event tap.
    ///
    /// `AXIsProcessTrusted()` alone is not that answer: it keeps returning `true`
    /// after the user *deletes* the app's row from System Settings → Privacy &
    /// Security → Accessibility, so a hook that believes it would never learn it
    /// had been revoked, would keep re-arming a tap macOS no longer lets it
    /// service, and would wedge clicks machine-wide until reboot (#674). Only
    /// creating a filtering tap tracks the live grant, so both are consulted: the
    /// trust read short-circuits the probe for a process that was never granted,
    /// which keeps a denied agent from asking `WindowServer` twice a second.
    fn has_accessibility() -> bool {
        // SAFETY: takes no arguments and only reads the current trust state — the
        // non-prompting counterpart of `AXIsProcessTrustedWithOptions`.
        let trusted = unsafe { AXIsProcessTrusted() };
        trusted && can_filter_events()
    }

    /// Raise the Accessibility prompt + register the process. See
    /// [`super::Hook::prompt_accessibility`].
    ///
    /// The `kAXTrustedCheckOptionPrompt = true` option is what makes macOS surface
    /// the dialog and list the process in System Settings; without it this is just
    /// [`Self::has_accessibility`].
    fn prompt_accessibility() {
        use objc2_application_services::kAXTrustedCheckOptionPrompt;
        use objc2_core_foundation::{CFDictionary, kCFBooleanTrue};

        // SAFETY: both are framework-provided constants, live for the process
        // lifetime; reading them copies a `&'static` reference.
        let (key, value) = unsafe { (kAXTrustedCheckOptionPrompt, kCFBooleanTrue) };
        let Some(value) = value else { return };
        let options = CFDictionary::from_slices(&[key], &[value]);
        // SAFETY: the dictionary holds exactly the documented key/value types
        // (`kAXTrustedCheckOptionPrompt` → `CFBoolean`). The returned trust state is
        // observed separately via the watcher, so it is deliberately dropped here.
        let _trusted = unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
    }

    /// See [`super::Hook::list_event_taps`].
    fn list_event_taps() -> Vec<EventTapInfo> {
        let mut count: u32 = 0;
        // SAFETY: a null `tap_list` with `max == 0` is the documented count-probe
        // form; it only writes `count`.
        let err = unsafe { CGGetEventTapList(0, std::ptr::null_mut(), &raw mut count) };
        if err != 0 || count == 0 {
            return Vec::new();
        }

        // SAFETY: `CGEventTapInformation` is a plain `repr(C)` POD; an all-zero bit
        // pattern is a valid instance (`enabled = false`, all numeric fields 0).
        // `CGGetEventTapList` overwrites each slot it fills.
        let mut taps: Vec<CGEventTapInformation> =
            vec![unsafe { std::mem::zeroed() }; count as usize];
        // SAFETY: `taps` holds exactly `count` initialised, correctly aligned slots
        // and stays alive for the call, and that same `count` is the maximum passed
        // in, so the C side cannot write past the allocation; the out-parameter
        // points at a live local it may only overwrite.
        let err = unsafe { CGGetEventTapList(count, taps.as_mut_ptr(), &raw mut count) };
        if err != 0 {
            return Vec::new();
        }
        // The second call may report fewer taps than the probe; never read past it.
        taps.truncate(count as usize);

        taps.into_iter()
            .map(|t| EventTapInfo {
                tap_id: t.event_tap_id,
                location: match t.tap_point {
                    0 => TapLocation::Hid,
                    1 => TapLocation::Session,
                    2 => TapLocation::AnnotatedSession,
                    other => TapLocation::Other(other),
                },
                // kCGEventTapOptionDefault == 0 (active); kCGEventTapOptionListenOnly == 1.
                active: t.options == 0,
                enabled: t.enabled,
                owner_pid: t.tapping_process,
                owner_name: process_name(t.tapping_process),
                target_pid: (t.process_being_tapped != 0).then_some(t.process_being_tapped),
            })
            .collect()
    }

    /// Read the frontmost application via `NSWorkspace`: its bundle identifier
    /// (the profile-matching key) and its localized name (for the UI). Returns
    /// `None` when no app is frontmost or it has no bundle identifier.
    ///
    /// `NSWorkspace` is `AnyThread`, so this is sound on the watcher thread. The
    /// reads return owned `Retained` values (no leak by construction), but the
    /// framework still autoreleases internal temporaries and `to_str` borrows its
    /// UTF-8 view from the pool — so an explicit `autoreleasepool` is required off
    /// the main thread, where no run loop drains one. (Without it the old raw path
    /// leaked the workspace/app/bundle-id objects: hundreds of MB across a workday.)
    fn frontmost_app() -> Option<ForegroundApp> {
        use objc2::rc::autoreleasepool;

        autoreleasepool(|pool| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication();
            observe_frontmost_application(app.as_deref(), pool)
        })
    }

    /// Read the global cursor position from a HID-state event source, which
    /// needs no tap and no permission.
    fn cursor_position() -> Option<CursorPosition> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
        let point = CGEvent::new(source).ok()?.location();
        Some(CursorPosition {
            x: point.x,
            y: point.y,
        })
    }
}

fn hooked_event_types() -> Vec<CGEventType> {
    vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        // Function-key remapper: F1–F12/Esc arrive as KeyDown/KeyUp.
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ]
}

/// Invoke the user callback under `catch_unwind`, always failing open.
fn run_tap_callback(
    cb: &dyn Fn(HookEvent) -> EventDisposition,
    etype: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // Mouse first, then keyboard; a given event type is one or the other.
        let hook_event = if let Some(mouse_event) = translate(etype, event) {
            HookEvent::Mouse(mouse_event)
        } else if let Some(key_event) = translate_key(etype, event) {
            HookEvent::Key(key_event)
        } else {
            return CallbackResult::Keep;
        };
        match cb(hook_event) {
            EventDisposition::PassThrough => CallbackResult::Keep,
            EventDisposition::Suppress => CallbackResult::Drop,
        }
    }));
    if let Ok(disposition) = result {
        disposition
    } else {
        error!(
            "OS mouse-hook callback panicked — passing event through to \
             avoid wedging system input"
        );
        CallbackResult::Keep
    }
}

/// Sibling watchdog: if the callback is still entered past the budget, abort
/// the agent so macOS tears the tap down and system input recovers.
fn spawn_callback_watchdog(
    signals: Arc<WatchdogSignals>,
    callback_activity: Arc<CallbackActivity>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("openlogi-hook-watchdog".into())
        .spawn(move || {
            loop {
                let phase = signals.phase();
                if matches!(phase, TapPhase::TapStopped | TapPhase::ThreadExited) {
                    return;
                }
                thread::sleep(CALLBACK_WATCHDOG_POLL_INTERVAL);
                let Some(entered) = callback_activity.entered_at_ms() else {
                    continue;
                };
                let Some(elapsed) = stuck_callback(signals.now_millis(), entered) else {
                    continue;
                };
                // Re-sample: a fresh high-frequency event may have rewritten
                // the complete activity state during the budget check.
                if callback_activity.entered_at_ms() != Some(entered) {
                    continue;
                }
                if signals.phase() != TapPhase::Armed {
                    continue;
                }
                error!(
                    stuck_ms = duration_millis(elapsed),
                    "OS mouse-hook callback stuck past budget — exiting agent to \
                     restore system input (HID CGEventTap freeze hazard)"
                );
                // A live callback owns the tap thread, so no in-process
                // teardown can make progress. Process death releases its Mach
                // port and removes the tap from the system event chain.
                #[expect(
                    clippy::exit,
                    reason = "this watchdog thread has no caller to return to and the stuck callback owns the active HID tap, which serialises every pointer event machine-wide; only process death makes macOS tear the tap down"
                )]
                std::process::exit(FREEZE_HAZARD_EXIT_CODE);
            }
        })
        .map(|_| ())
}

/// Independent lifecycle watchdog for paths that never enter the Rust tap
/// callback (for example, TCC revocation wedging `CFRunLoopRunInMode` or the
/// Accessibility query itself).
///
/// This thread is started before the tap thread, uses only atomics and a
/// monotonic clock, and remains armed after a stop request. It deliberately
/// does not call `has_accessibility()`: that query can itself stop returning
/// after TCC revocation. It disarms only after the tap thread reports that the
/// tap was synchronously disabled and destroyed, or (for explicit shutdown)
/// after that thread has exited.
fn spawn_lifecycle_watchdog(
    signals: Arc<WatchdogSignals>,
) -> Result<thread::JoinHandle<()>, HookError> {
    thread::Builder::new()
        .name("openlogi-hook-lifecycle-watchdog".into())
        .spawn(move || {
            let mut watchdog = LifecycleWatchdog::default();
            loop {
                let observation = LifecycleObservation {
                    phase: signals.phase(),
                    stop_requested: signals.stop_requested(),
                    tap_progress_at: signals.tap_progress_at(),
                };
                match watchdog.evaluate(signals.now(), observation) {
                    LifecycleDecision::Continue => {
                        thread::park_timeout(LIFECYCLE_WATCHDOG_POLL_INTERVAL);
                    }
                    LifecycleDecision::Complete => return,
                    LifecycleDecision::Exit { reason, elapsed } => {
                        // The tap thread may have completed immediately after
                        // the decision. Only a still-hazardous phase may exit.
                        let phase = signals.phase();
                        let still_hazardous = match reason {
                            LifecycleExitReason::TapThreadStalled => {
                                matches!(phase, TapPhase::Arming | TapPhase::Armed)
                            }
                            LifecycleExitReason::StopTimedOut => phase != TapPhase::ThreadExited,
                        };
                        if !still_hazardous {
                            continue;
                        }
                        let reason = match reason {
                            LifecycleExitReason::TapThreadStalled if phase == TapPhase::Arming => {
                                "HID tap creation or activation stopped making progress"
                            }
                            LifecycleExitReason::TapThreadStalled => {
                                "HID tap thread stopped making progress while tap remained active"
                            }
                            LifecycleExitReason::StopTimedOut => {
                                "hook stop requested but tap thread did not exit"
                            }
                        };
                        error!(
                            reason,
                            elapsed_ms = duration_millis(elapsed),
                            ?phase,
                            "HID CGEventTap lifecycle did not make progress before deadline — \
                             exiting agent to restore system input"
                        );
                        #[expect(
                            clippy::exit,
                            reason = "the tap thread is wedged (TCC revocation can stall it inside CoreGraphics), so no unwinding path can reach it from this watchdog thread; a live HID tap left behind freezes all input until the process dies"
                        )]
                        std::process::exit(FREEZE_HAZARD_EXIT_CODE);
                    }
                }
            }
        })
        .map_err(|error| HookError::MacOsTap(format!("could not spawn tap watchdog: {error}")))
}

/// Service the tap until it has to be released: an explicit stop, a stopped run
/// loop, a revoked permission, or a tap the OS will not keep enabled.
fn service_tap(tap: &CGEventTap<'_>, signals: &WatchdogSignals, tap_disabled: &AtomicBool) {
    // Service the tap in short slices instead of an unbounded
    // `run_current()`. Between slices we re-check that we may still filter
    // events: an active tap at the HID location that outlives its permission
    // wedges the *entire* system input stream — mouse and keyboard alike —
    // until reboot. If the user revokes access while we're live, tear the tap
    // down right here, on the tap's own thread, so input is restored even
    // when the UI thread is already stuck.
    //
    // `stop()` requests shutdown two ways: it sets the stop latch and calls
    // `run_loop.stop()`. The CF stop returns `Stopped` and breaks promptly
    // while a slice is running, but is a no-op if it lands in the gap
    // between slices (CFRunLoopStop only acts on a running loop). The latch,
    // checked at the top of every slice, is the reliable signal: in
    // that race the thread notices one 500 ms slice later instead of joining
    // forever.
    let mut rearm = RearmBudget::default();
    loop {
        if signals.stop_requested() {
            break;
        }
        signals.mark_tap_progress();
        match CFRunLoop::run_in_mode(
            // SAFETY: framework-provided static CFStringRef, 'static.
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(500),
            false,
        ) {
            CFRunLoopRunResult::Stopped | CFRunLoopRunResult::Finished => break,
            CFRunLoopRunResult::TimedOut | CFRunLoopRunResult::HandledSource => {}
        }
        signals.mark_tap_progress();
        if !Backend::has_accessibility() {
            warn!(
                "Accessibility revoked while the event tap was live — \
                 disabling the tap to avoid wedging system input"
            );
            break;
        }
        // Observe both disable signals: the callback catches the documented
        // TapDisabledBy* notification, while the port state catches the
        // sleep/wake edge where CoreGraphics disables the tap without one.
        // Either one consumes the same bounded re-arm budget.
        let was_disabled = tap_disabled.swap(false, Ordering::AcqRel) || !tap_is_enabled(tap);
        if was_disabled && !rearm.allow(signals.now()) {
            error!(
                "the OS keeps disabling the HID tap — releasing it instead of \
                 re-arming a tap nothing is servicing"
            );
            break;
        }
        // Enabling is idempotent while the tap is already live. Only reached
        // while the live capability probe above still succeeds.
        tap.enable();
    }
}

/// Body of the background hook thread.
#[expect(
    clippy::needless_pass_by_value,
    reason = "rl_tx must be owned: dropping it signals the parent's recv() to return Err on failure paths"
)]
fn thread_main(
    cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync>,
    rl_tx: mpsc::Sender<CFRunLoop>,
    signals: Arc<WatchdogSignals>,
) {
    // Declared first so it drops last, after the tap, callback, source, and run
    // loop locals have unwound. The lifecycle watchdog treats this notification
    // as proof that an explicit stop has completed, not merely been requested.
    let _thread_exit = signals.thread_exit_guard();

    // A successful CGEventTapCreate may install the HID tap before returning,
    // so lifecycle monitoring must be armed before entering CoreGraphics.
    signals.mark_tap_progress();
    signals.set_phase(TapPhase::Arming);

    let callback_activity = Arc::new(CallbackActivity::default());
    // Latched by the callback when the OS disables the tap, consumed by the
    // run-loop slice that decides whether to re-arm it.
    let tap_disabled = Arc::new(AtomicBool::new(false));

    let tap_result = {
        let callback_signals = Arc::clone(&signals);
        let callback_activity = Arc::clone(&callback_activity);
        let tap_disabled = Arc::clone(&tap_disabled);
        CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            hooked_event_types(),
            move |_proxy: CGEventTapProxy, etype: CGEventType, event: &CGEvent| {
                if matches!(
                    etype,
                    CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                ) {
                    tap_disabled.store(true, Ordering::Release);
                }
                callback_activity.enter(callback_signals.now_millis());
                let disposition = run_tap_callback(cb.as_ref(), etype, event);
                callback_activity.exit();
                disposition
            },
        )
    };

    let Ok(tap) = tap_result else {
        error!("CGEventTapCreate returned null — Accessibility may have been revoked");
        // Dropping rl_tx causes rl_rx.recv() on the parent to return Err,
        // which we surface as MacOsTap.
        return;
    };
    signals.mark_tap_progress();

    let Ok(loop_source) = tap.mach_port().create_runloop_source(0) else {
        error!("CFRunLoopSourceCreate failed for event tap");
        return;
    };
    signals.mark_tap_progress();

    let run_loop = CFRunLoop::get_current();

    // SAFETY: kCFRunLoopCommonModes is a static CF string constant that
    // lives for the duration of the process.
    unsafe {
        run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }
    signals.mark_tap_progress();
    if let Err(error) =
        spawn_callback_watchdog(Arc::clone(&signals), Arc::clone(&callback_activity))
    {
        error!(%error, "could not spawn callback watchdog — refusing to arm HID tap");
        return;
    }
    signals.mark_tap_progress();
    tap.enable();
    signals.mark_tap_progress();
    signals.set_phase(TapPhase::Armed);

    if rl_tx.send(run_loop.clone()).is_err() {
        debug!("hook parent dropped before run loop was ready; stopping");
        disable_tap(&tap);
        // SAFETY: framework-provided static CFStringRef, 'static.
        run_loop.remove_source(&loop_source, unsafe { kCFRunLoopCommonModes });
        drop(loop_source);
        drop(tap);
        signals.set_phase(TapPhase::TapStopped);
        return;
    }

    service_tap(&tap, &signals, &tap_disabled);

    // Detach the tap from the event stream synchronously before unwinding,
    // so input recovers immediately rather than whenever CF happens to
    // release the port.
    disable_tap(&tap);
    // SAFETY: framework-provided static CFStringRef, 'static.
    run_loop.remove_source(&loop_source, unsafe { kCFRunLoopCommonModes });
    drop(loop_source);
    drop(tap);
    signals.set_phase(TapPhase::TapStopped);
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Whether CoreGraphics currently considers `tap` enabled. Checked on the tap
/// thread so an unreported OS disable consumes the same budget as a callback.
fn tap_is_enabled(tap: &CGEventTap<'_>) -> bool {
    use core_foundation::base::TCFType as _;

    // SAFETY: the port is owned by `tap` and remains live for this call.
    unsafe { CGEventTapIsEnabled(tap.mach_port().as_concrete_TypeRef()) }
}

/// Disable an active tap synchronously. Dropping `CGEventTap` then invalidates
/// its Mach port on the same thread.
fn disable_tap(tap: &CGEventTap<'_>) {
    use core_foundation::base::TCFType as _;

    // SAFETY: the port is owned by `tap` and remains live for this call;
    // disabling is idempotent.
    unsafe { CGEventTapEnable(tap.mach_port().as_concrete_TypeRef(), false) };
}

/// Mirror of CoreGraphics' `CGEventTapInformation`. `#[repr(C)]` reproduces the
/// header layout (including the padding before `events_of_interest` and
/// `min_usec_latency`) so `CGGetEventTapList` writes into the right offsets.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGEventTapInformation {
    event_tap_id: u32,
    tap_point: u32,
    options: u32,
    events_of_interest: u64,
    tapping_process: i32,
    process_being_tapped: i32,
    enabled: bool,
    min_usec_latency: f32,
    avg_usec_latency: f32,
    max_usec_latency: f32,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // `core-graphics` doesn't bind the enumeration side (it ships the tap
    // *create/enable* path only), so we declare it ourselves. Passing a null
    // list with count 0 returns the number of taps via `event_tap_count`.
    fn CGGetEventTapList(
        max_number_of_taps: u32,
        tap_list: *mut CGEventTapInformation,
        event_tap_count: *mut u32,
    ) -> i32;
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    // libproc; resolves a PID to its executable path. Returns the byte length
    // written, or <= 0 on failure (e.g. the process exited, or it's out of the
    // caller's permission scope).
    fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffersize: u32) -> i32;
}

/// Best-effort PID → executable file name via libproc.
fn process_name(pid: i32) -> Option<String> {
    // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN (4 * 1024).
    const BUF_LEN: u32 = 4096;
    if pid <= 0 {
        return None;
    }
    let mut buf = vec![0u8; BUF_LEN as usize];
    // SAFETY: `buf` is a live, writable buffer of `BUF_LEN` bytes; the C side
    // writes at most that many and returns the length actually written.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), BUF_LEN) };
    if len <= 0 {
        return None;
    }
    // `len > 0` here, so `unsigned_abs` is the value itself; widening to usize
    // is lossless and sidesteps the sign-loss cast lint.
    buf.truncate(len.unsigned_abs() as usize);
    let path = String::from_utf8_lossy(&buf);
    Some(path.rsplit('/').next().unwrap_or(&path).to_string())
}

#[cfg(test)]
mod tests {
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::*;

    #[test]
    fn tap_callback_suppresses_normally_and_passes_through_panics() {
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .expect("CGEventSourceCreate must succeed");
        let event = CGEvent::new(source).expect("CGEventCreate must succeed");

        assert!(matches!(
            run_tap_callback(
                &|_| EventDisposition::Suppress,
                CGEventType::MouseMoved,
                &event
            ),
            CallbackResult::Drop
        ));
        assert!(matches!(
            run_tap_callback(
                &|_| panic!("test callback panic"),
                CGEventType::MouseMoved,
                &event
            ),
            CallbackResult::Keep
        ));
    }
}

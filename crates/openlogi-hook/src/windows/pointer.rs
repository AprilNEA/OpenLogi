//! Read-only native HWND hit-testing, exact foreground identity checks, and the
//! activity latch that lets a pointer-context watcher sleep between changes.
#![expect(unsafe_code, reason = "read-only Win32 window and cursor queries")]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, Thread};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetDesktopWindow, GetForegroundWindow,
    GetShellWindow, GetWindowThreadProcessId, WindowFromPoint,
};

use super::hook::application_for_process;
use crate::{PointerContext, PointerTarget};

/// Set by the native input callbacks when the window under the pointer may
/// have changed; cleared by [`PointerContext::wait_for_activity`].
static ACTIVITY: AtomicBool = AtomicBool::new(false);

/// The thread parked in [`PointerContext::wait_for_activity`].
static WAITER: Mutex<Option<Thread>> = Mutex::new(None);

/// Whether a running `WH_MOUSE_LL` hook reports mouse input to [`note_activity`].
static MOTION_TRACKED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Whether this thread already owns [`WAITER`], so a steady-state wait
    /// never takes the lock the hook callback tries.
    static REGISTERED_WAITER: Cell<bool> = const { Cell::new(false) };
}

/// The previous lookup's root window, keyed with its owning PID so a recycled
/// HWND cannot inherit another process's identity.
struct Resolved {
    window: usize,
    process_id: Option<i32>,
    context: PointerContext,
}

/// Pointer motion within one window repeats the same lookup; this skips the
/// desktop check and the process image query until the root window changes.
static LAST_RESOLVED: Mutex<Option<Resolved>> = Mutex::new(None);

pub(crate) fn pointer_context_supported() -> bool {
    true
}

/// Record that the window under the pointer may have changed and wake the
/// waiter. Runs on native input callbacks, so it never allocates or blocks:
/// once the latch is set, each further event costs one relaxed load, and only
/// the first event after a wait touches the waiter slot (with `try_lock`).
pub(super) fn note_activity() {
    if ACTIVITY.load(Ordering::Relaxed) || ACTIVITY.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(waiter) = WAITER.try_lock()
        && let Some(thread) = waiter.as_ref()
    {
        thread.unpark();
    }
}

/// Marks mouse input as reaching [`note_activity`] for as long as the
/// installed `WH_MOUSE_LL` hook that holds it lives.
pub(super) struct MotionTracking(());

impl MotionTracking {
    pub(super) fn start() -> Self {
        MOTION_TRACKED.store(true, Ordering::Relaxed);
        Self(())
    }
}

impl Drop for MotionTracking {
    fn drop(&mut self) {
        MOTION_TRACKED.store(false, Ordering::Relaxed);
    }
}

impl PointerContext {
    /// **Windows only.** Park the calling thread until the window under the
    /// pointer may have changed — the input hook saw the pointer move or a
    /// button change, or the foreground window changed — or until `timeout`
    /// elapses. Returns whether activity ended the wait; activity since the
    /// previous wait returns at once.
    ///
    /// An external [`Thread::unpark`] (for example, a profile-mode change) or
    /// a spurious wake also ends the wait, returning false without activity.
    /// The caller can then recheck its lifecycle and configuration.
    ///
    /// Lets a [`pointer_context`](crate::pointer_context) watcher sleep while
    /// the pointer rests instead of polling. The input hook runs before Windows
    /// applies the input, so a query right after a wake can still see the
    /// previous window; re-check once shortly after activity stops. Mouse input
    /// is seen only while a [`Hook`](crate::Hook) runs
    /// ([`Self::motion_is_tracked`]). Meant for one waiting thread: a second
    /// waiter takes the wake-ups over and leaves the first to its timeout.
    pub fn wait_for_activity(timeout: Duration) -> bool {
        REGISTERED_WAITER.with(|registered| {
            if !registered.replace(true) {
                *WAITER.lock().unwrap_or_else(PoisonError::into_inner) = Some(thread::current());
            }
        });
        if ACTIVITY.swap(false, Ordering::AcqRel) {
            return true;
        }
        thread::park_timeout(timeout);
        ACTIVITY.swap(false, Ordering::AcqRel)
    }

    /// **Windows only.** Whether this session can read the cursor at all. It
    /// cannot while another desktop takes the input — the lock screen, a UAC
    /// prompt, a disconnected remote session — and every
    /// [`pointer_context`](crate::pointer_context) lookup then reports
    /// [`PointerTarget::Unavailable`], for as long as the user is away.
    #[must_use]
    pub fn cursor_is_readable() -> bool {
        let mut point = POINT::default();
        // SAFETY: point is a live writable out-pointer.
        unsafe { GetCursorPos(&raw mut point) != 0 }
    }

    /// **Windows only.** Whether mouse input currently reaches
    /// [`Self::wait_for_activity`]: true while a [`Hook`](crate::Hook) runs.
    /// Without one only foreground changes wake a waiter, so a watcher has to
    /// poll instead.
    #[must_use]
    pub fn motion_is_tracked() -> bool {
        MOTION_TRACKED.load(Ordering::Relaxed)
    }
}

fn process_id(window: HWND) -> Option<i32> {
    if window.is_null() {
        return None;
    }
    let mut pid = 0;
    // SAFETY: window is a native handle; pid is a live writable out-pointer.
    if unsafe { GetWindowThreadProcessId(window, &raw mut pid) } == 0 {
        return None;
    }
    i32::try_from(pid).ok().filter(|pid| *pid > 0)
}

fn window_target(window: HWND) -> Option<PointerTarget> {
    Some(PointerTarget::Window {
        process_id: process_id(window)?,
        window_id: window as usize as u64,
    })
}

fn is_desktop(window: HWND) -> bool {
    // SAFETY: these getters have no preconditions and do not acquire ownership.
    let desktop = unsafe { GetDesktopWindow() };
    if window == desktop {
        return true;
    }
    // SAFETY: GetShellWindow has no preconditions.
    let shell = unsafe { GetShellWindow() };
    if shell.is_null() {
        return false;
    }
    if window == shell {
        return true;
    }
    // Explorer hosts desktop wallpaper/icons in WorkerW, but also owns ordinary
    // folders and the taskbar. Require both shell ownership and desktop class.
    let Some(shell_pid) = process_id(shell) else {
        return false;
    };
    if process_id(window) != Some(shell_pid) {
        return false;
    }
    matches!(class_name(window).as_deref(), Some("WorkerW" | "Progman"))
}

fn class_name(window: HWND) -> Option<String> {
    let mut class = [0u16; 128];
    // SAFETY: class is a live buffer of 128 code units, window is a native handle.
    let len = unsafe { GetClassNameW(window, class.as_mut_ptr(), 128) };
    let len = usize::try_from(len).ok().filter(|len| *len > 0)?;
    Some(String::from_utf16_lossy(&class[..len]))
}

pub(crate) fn pointer_context() -> Option<PointerContext> {
    let mut point = POINT::default();
    // SAFETY: point is a live writable out-pointer. Use the same unscaled
    // Win32 coordinates for WindowFromPoint, not GPUI's logical cursor position.
    if unsafe { GetCursorPos(&raw mut point) } == 0 {
        return None;
    }
    // SAFETY: point is passed by value in screen coordinates.
    let child = unsafe { WindowFromPoint(point) };
    if child.is_null() {
        return None;
    }
    // SAFETY: child is a native HWND; GA_ROOT preserves owned popup identity.
    let window = unsafe { GetAncestor(child, GA_ROOT) };
    if window.is_null() {
        return None;
    }
    let process_id = process_id(window);
    if let Some(last) = LAST_RESOLVED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        && last.window == window as usize
        && last.process_id == process_id
    {
        return Some(last.context.clone());
    }
    let context = resolve(window, process_id)?;
    *LAST_RESOLVED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Resolved {
        window: window as usize,
        process_id,
        context: context.clone(),
    });
    Some(context)
}

/// Classify a root window the cache has not seen under this owner.
fn resolve(window: HWND, process_id: Option<i32>) -> Option<PointerContext> {
    if is_desktop(window) {
        return Some(PointerContext {
            app: None,
            target: PointerTarget::Desktop,
        });
    }
    let process_id = process_id?;
    Some(PointerContext {
        app: Some(application_for_process(process_id.cast_unsigned())?),
        target: PointerTarget::Window {
            process_id,
            window_id: window as usize as u64,
        },
    })
}

pub(crate) fn pointer_target_is_focused(expected: PointerTarget) -> bool {
    // SAFETY: GetForegroundWindow has no preconditions and does not change focus.
    let foreground = unsafe { GetForegroundWindow() };
    let focused = window_target(foreground);
    if focused == Some(expected) {
        return true;
    }
    // Name both windows, so a log tells another application apart from
    // another window of the same one. Field expressions only run when debug
    // logging is on, so the class lookups cost nothing otherwise.
    let hovered_class = || match expected {
        PointerTarget::Window { window_id, .. } => usize::try_from(window_id)
            .ok()
            .and_then(|id| class_name(std::ptr::without_provenance_mut(id))),
        _ => None,
    };
    tracing::debug!(
        ?expected,
        hovered_class = ?hovered_class(),
        foreground = ?focused,
        foreground_class = ?class_name(foreground),
        "window under the pointer is not the foreground window"
    );
    false
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// One test owns the process-global latch, so no parallel test consumes
    /// its activity. A parallel test's live hook can only add activity, which
    /// no assertion here mistakes for a missing wake.
    #[test]
    fn activity_wakes_the_waiter_and_is_consumed_once() {
        note_activity();
        assert!(
            PointerContext::wait_for_activity(Duration::from_secs(5)),
            "activity before the wait returns at once"
        );
        assert!(
            !ACTIVITY.load(Ordering::Acquire),
            "the wait consumed the activity it returned for"
        );

        let notifier = thread::spawn(|| {
            thread::sleep(Duration::from_millis(50));
            note_activity();
        });
        let started = Instant::now();
        assert!(PointerContext::wait_for_activity(Duration::from_secs(10)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the notifier unparked the waiter instead of the timeout ending it"
        );
        notifier.join().unwrap();

        // A profile resume uses the same unpark token as a wake while the
        // worker is already parked, without setting the native-activity latch.
        thread::current().unpark();
        let started = Instant::now();
        assert!(
            !PointerContext::wait_for_activity(Duration::from_secs(1)),
            "a profile resume does not manufacture native input"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "an external unpark must return control instead of waiting for the deadline"
        );
    }
}

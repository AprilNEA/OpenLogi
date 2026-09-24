//! Blocking, read-only pointer-window queries, independent of foreground caches.

use crate::ForegroundApp;

#[cfg(target_os = "linux")]
use crate::linux::pointer as platform;
#[cfg(target_os = "macos")]
use crate::macos::pointer as platform;
#[cfg(target_os = "windows")]
use crate::windows::pointer as platform;

/// Native window identity under the pointer, or an explicit non-window outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerTarget {
    /// A window identified by both its owner and native window-server handle.
    Window {
        /// Native process identifier.
        process_id: i32,
        /// CGWindowID, HWND, or X11 client-window XID. Not durable across restarts.
        window_id: u64,
    },
    /// Positively identified desktop background or desktop icons.
    Desktop,
    /// A query failed, or the pointer is over an unidentifiable surface.
    Unavailable,
    /// This session cannot expose native pointer-window identity (e.g. Wayland).
    Unsupported,
}

/// Application identity and native window captured by one pointer lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerContext {
    /// Uses exactly the same profile ID namespace as the foreground app lookup.
    /// Absent for desktop, unsupported sessions, and unavailable lookups.
    pub app: Option<ForegroundApp>,
    /// Native target. Missing application metadata never becomes `Desktop`.
    pub target: PointerTarget,
}

/// Query the window currently under the pointer, without activating anything.
///
/// Blocking window-server / accessibility I/O: call only on a background worker,
/// never an input hook or event-tap callback. This does not update foreground
/// caches (including the foreground Safari PID).
#[must_use]
pub fn pointer_context() -> PointerContext {
    if !pointer_context_supported() {
        return PointerContext {
            app: None,
            target: PointerTarget::Unsupported,
        };
    }
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    if let Some(context) = platform::pointer_context() {
        return context;
    }
    PointerContext {
        app: None,
        target: PointerTarget::Unavailable,
    }
}

/// Whether this session has a pointer-window backend; does not probe permissions
/// or connect to the window server. Unsupported Wayland never falls back to X11.
#[must_use]
pub fn pointer_context_supported() -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    return platform::pointer_context_supported();
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    false
}

/// Verify that this exact window, not merely another window of its application,
/// is focused now. Returns false on any lookup failure or non-window target.
///
/// Like [`pointer_context`], this may block and must never run in an input hook.
/// It only observes focus and never changes it.
#[must_use]
pub fn pointer_target_is_focused(target: PointerTarget) -> bool {
    if !matches!(target, PointerTarget::Window { .. }) || !pointer_context_supported() {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    return platform::pointer_target_is_focused(target);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    false
}

#[cfg(any(target_os = "macos", test))]
pub(crate) mod hit_test;

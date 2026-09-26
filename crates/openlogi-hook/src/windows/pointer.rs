//! Read-only native HWND hit-testing and exact foreground identity checks.
#![expect(unsafe_code, reason = "read-only Win32 window and cursor queries")]

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetDesktopWindow, GetForegroundWindow,
    GetShellWindow, GetWindowThreadProcessId, WindowFromPoint,
};

use super::hook::application_for_process;
use crate::{PointerContext, PointerTarget};

pub(crate) fn pointer_context_supported() -> bool {
    true
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
    let mut class = [0u16; 128];
    // SAFETY: class is a live buffer of 128 code units, window is a native handle.
    let len = unsafe { GetClassNameW(window, class.as_mut_ptr(), 128) };
    let Ok(len) = usize::try_from(len) else {
        return false;
    };
    matches!(
        String::from_utf16_lossy(&class[..len]).as_str(),
        "WorkerW" | "Progman"
    )
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
    if is_desktop(window) {
        return Some(PointerContext {
            app: None,
            target: PointerTarget::Desktop,
        });
    }
    let target = window_target(window)?;
    let PointerTarget::Window { process_id, .. } = target else {
        return None;
    };
    Some(PointerContext {
        app: Some(application_for_process(process_id.cast_unsigned())?),
        target,
    })
}

pub(crate) fn pointer_target_is_focused(expected: PointerTarget) -> bool {
    // SAFETY: GetForegroundWindow has no preconditions and does not change focus.
    window_target(unsafe { GetForegroundWindow() }) == Some(expected)
}

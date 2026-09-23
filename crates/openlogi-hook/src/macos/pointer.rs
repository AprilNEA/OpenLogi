//! Off-tap WindowServer hit testing and read-only AX focused-window validation.

use std::ptr::NonNull;
use std::sync::LazyLock;
use std::sync::mpsc;
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSRunningApplication, NSWindow, NSWorkspace};
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGWindowLevelForKey, CGWindowLevelKey, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};

use super::foreground::foreground_app_from_running_application;
use crate::{ForegroundApp, PointerContext, PointerTarget};

type Dictionary = CFDictionary<CFString, CFType>;

const MAIN_THREAD_HIT_TEST_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) fn pointer_context_supported() -> bool {
    true
}

fn dictionary(value: CFRetained<CFType>) -> Option<CFRetained<Dictionary>> {
    let value = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: CGWindowList dictionaries have CFString keys and CF-object
    // values. Each value is downcast separately before use.
    Some(unsafe { CFRetained::cast_unchecked::<Dictionary>(value) })
}

fn number(info: &Dictionary, key: &CFString) -> Option<CFRetained<CFNumber>> {
    info.get(key)?.downcast::<CFNumber>().ok()
}

/// Dock and WindowManager draw Mission Control, App Exposé and Stage Manager.
const SYSTEM_OVERLAY_OWNERS: [&str; 2] = ["com.apple.dock", "com.apple.WindowManager"];

fn overlay_target(owner: Option<&ForegroundApp>) -> PointerTarget {
    if owner.is_some_and(|app| SYSTEM_OVERLAY_OWNERS.contains(&app.id.as_str())) {
        PointerTarget::Desktop
    } else {
        PointerTarget::Unavailable
    }
}

fn target(info: &Dictionary, owner: Option<&ForegroundApp>) -> Option<PointerTarget> {
    // SAFETY: immutable Core Graphics string constant.
    let layer = number(info, unsafe { kCGWindowLayer })?.as_i32()?;
    if layer == CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey)
        || layer == CGWindowLevelForKey(CGWindowLevelKey::DesktopIconWindowLevelKey)
    {
        return Some(PointerTarget::Desktop);
    }
    if layer != CGWindowLevelForKey(CGWindowLevelKey::NormalWindowLevelKey) {
        return Some(overlay_target(owner));
    }
    let process_id = owner_pid(info)?;
    // SAFETY: immutable Core Graphics string constant.
    let window_id = number(info, unsafe { kCGWindowNumber })?.as_i64()?;
    if window_id <= 0 {
        return None;
    }
    Some(PointerTarget::Window {
        process_id,
        window_id: u64::try_from(window_id).ok()?,
    })
}

fn owner_pid(info: &Dictionary) -> Option<i32> {
    // SAFETY: immutable Core Graphics string constant.
    let pid = number(info, unsafe { kCGWindowOwnerPID })?.as_i32()?;
    (pid > 0).then_some(pid)
}

fn owner_app(process_id: i32) -> Option<ForegroundApp> {
    // This background worker has no AppKit run loop to drain temporaries.
    // Reuse only the pure conversion; never publish a foreground Safari PID.
    objc2::rc::autoreleasepool(|pool| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)?;
        foreground_app_from_running_application(&app, pool)
    })
}

/// AppKit's hit test skips click-through overlays that a `CGWindowList` scan
/// cannot distinguish, such as macOS 27's display-sized Notification Center
/// window. It is main-thread-only, so hop to the agent's AppKit loop.
fn window_number_under_pointer() -> Option<isize> {
    let (tx, rx) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let _ = tx.send(NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(
            NSEvent::mouseLocation(),
            0,
            mtm,
        ));
    });
    rx.recv_timeout(MAIN_THREAD_HIT_TEST_TIMEOUT).ok()
}

pub(crate) fn pointer_context() -> Option<PointerContext> {
    let window_number = window_number_under_pointer()?;
    let window_id = u32::try_from(window_number).ok().filter(|id| *id != 0)?;
    let windows = CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, window_id)?;
    // SAFETY: CGWindowListCopyWindowInfo returns an array of CF dictionaries.
    let windows = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(windows) };
    let info = dictionary(windows.into_iter().next()?)?;
    let owner = owner_pid(&info).and_then(owner_app);
    let target = target(&info, owner.as_ref())?;
    let app = match target {
        PointerTarget::Window { .. } => Some(owner?),
        _ => None,
    };
    Some(PointerContext { app, target })
}

// No public API joins AXUIElement and CGWindowID. This long-lived private SPI
// is also used by Hammerspoon/yabai, but absence must never prevent app launch.
type WindowIdFn = unsafe extern "C" fn(*const AXUIElement, *mut u32) -> AXError;
static WINDOW_ID: LazyLock<Option<WindowIdFn>> = LazyLock::new(|| {
    // SAFETY: lookup in already loaded frameworks using a NUL-terminated name;
    // RTLD_DEFAULT does not acquire a handle or require a matching dlclose.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_AXUIElementGetWindow".as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: this SPI has the AXError(AXUIElementRef, CGWindowID*) C ABI.
    Some(unsafe { std::mem::transmute::<*mut libc::c_void, WindowIdFn>(symbol) })
});

pub(crate) fn pointer_target_is_focused(target: PointerTarget) -> bool {
    let PointerTarget::Window {
        process_id,
        window_id,
    } = target
    else {
        return false;
    };
    focused_window_id(process_id) == Some(window_id)
}

fn focused_window_id(pid: i32) -> Option<u64> {
    let get_window_id = (*WINDOW_ID)?;
    objc2::rc::autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        if workspace.frontmostApplication()?.processIdentifier() != pid {
            return None;
        }
        // SAFETY: a positive PID identifies the current frontmost process.
        let app = unsafe { AXUIElement::new_application(pid) };
        // SAFETY: this live app element gets a local 100 ms timeout, not a
        // process-global override that could change another AX consumer.
        if unsafe { app.set_messaging_timeout(0.1) } != AXError::Success {
            return None;
        }
        let mut value = std::ptr::null();
        let attr = CFString::from_static_str("AXFocusedWindow");
        // SAFETY: attr and app are live, value is a writable Copy-rule output.
        if unsafe { app.copy_attribute_value(&attr, NonNull::from(&mut value)) } != AXError::Success
        {
            return None;
        }
        let value = NonNull::new(value.cast_mut())?;
        // SAFETY: AX Copy success returns an owned CF object; adopt exactly once.
        let value = unsafe { CFRetained::from_raw(value) };
        let window = value.downcast::<AXUIElement>().ok()?;
        // SAFETY: timeout applies to this live retained element only.
        if unsafe { window.set_messaging_timeout(0.1) } != AXError::Success {
            return None;
        }
        let mut id = 0;
        // SAFETY: window is a retained AX window and id is a writable u32.
        if unsafe { get_window_id(&raw const *window, &raw mut id) } != AXError::Success || id == 0
        {
            return None;
        }
        // AX calls can block: reject a process switch during the lookup.
        if workspace.frontmostApplication()?.processIdentifier() != pid {
            return None;
        }
        Some(u64::from(id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> ForegroundApp {
        ForegroundApp {
            id: id.into(),
            display_name: id.into(),
        }
    }

    #[test]
    fn mission_control_overlays_count_as_desktop() {
        for owner in SYSTEM_OVERLAY_OWNERS {
            assert_eq!(overlay_target(Some(&app(owner))), PointerTarget::Desktop);
        }
    }

    #[test]
    fn other_overlays_stay_unavailable() {
        assert_eq!(
            overlay_target(Some(&app("com.apple.notificationcenterui"))),
            PointerTarget::Unavailable
        );
        assert_eq!(overlay_target(None), PointerTarget::Unavailable);
    }
}

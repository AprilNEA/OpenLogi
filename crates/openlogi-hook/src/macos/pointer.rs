//! Off-tap WindowServer hit testing and read-only AX focused-window validation.

use std::ptr::NonNull;
use std::sync::LazyLock;

use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGWindowLevelForKey, CGWindowLevelKey, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};

use super::foreground::foreground_app_from_running_application;
use crate::pointer::hit_test::{Window, hit_test};
use crate::{HookBackend as _, PointerContext, PointerTarget};

type Dictionary = CFDictionary<CFString, CFType>;

pub(crate) fn pointer_context_supported() -> bool {
    true
}

fn dictionary(value: CFRetained<CFType>) -> Option<CFRetained<Dictionary>> {
    let value = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: CGWindowList dictionaries (including bounds) have CFString keys
    // and CF-object values. Each value is downcast separately before use.
    Some(unsafe { CFRetained::cast_unchecked::<Dictionary>(value) })
}

fn number(info: &Dictionary, key: &CFString) -> Option<CFRetained<CFNumber>> {
    info.get(key)?.downcast::<CFNumber>().ok()
}

fn window(info: CFRetained<CFType>) -> Option<(Window, f64)> {
    let info = dictionary(info)?;
    // SAFETY: immutable Core Graphics string constants have process lifetime.
    let bounds_key = unsafe { kCGWindowBounds };
    let bounds = dictionary(info.get(bounds_key)?)?;
    // SAFETY: immutable Core Graphics string constant.
    let layer = number(&info, unsafe { kCGWindowLayer })?.as_i32()?;
    // SAFETY: immutable Core Graphics string constant.
    let alpha = number(&info, unsafe { kCGWindowAlpha })?.as_f64()?;
    let target = if layer == CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey)
        || layer == CGWindowLevelForKey(CGWindowLevelKey::DesktopIconWindowLevelKey)
    {
        PointerTarget::Desktop
    } else if layer == CGWindowLevelForKey(CGWindowLevelKey::NormalWindowLevelKey) {
        // SAFETY: immutable Core Graphics string constant.
        let process_id = number(&info, unsafe { kCGWindowOwnerPID })?.as_i32()?;
        // SAFETY: immutable Core Graphics string constant.
        let window_id = number(&info, unsafe { kCGWindowNumber })?.as_i64()?;
        if process_id <= 0 || window_id <= 0 {
            return None;
        }
        PointerTarget::Window {
            process_id,
            window_id: u64::try_from(window_id).ok()?,
        }
    } else {
        // Menus, Dock, floating panels and other overlays are obstacles, not
        // permission to select an application or desktop beneath them.
        PointerTarget::Unavailable
    };
    Some((
        Window {
            x: number(&bounds, &CFString::from_static_str("X"))?.as_f64()?,
            y: number(&bounds, &CFString::from_static_str("Y"))?.as_f64()?,
            width: number(&bounds, &CFString::from_static_str("Width"))?.as_f64()?,
            height: number(&bounds, &CFString::from_static_str("Height"))?.as_f64()?,
            target,
        },
        alpha,
    ))
}

pub(crate) fn pointer_context() -> Option<PointerContext> {
    let point = super::Backend::cursor_position()?;
    // OnScreenOnly is documented front-to-back. Retain desktop elements so
    // Desktop requires an actual desktop-layer hit, not an empty/failed list.
    let windows = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0)?;
    // SAFETY: CGWindowListCopyWindowInfo returns an array of CF dictionaries.
    let windows = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(windows) };
    let candidates = windows.into_iter().filter_map(|info| match window(info) {
        Some((_, alpha)) if alpha <= 0.0 => None,
        Some((window, _)) => Some(Some(window)),
        None => Some(None),
    });
    let target = hit_test(point, candidates);
    let app = if let PointerTarget::Window { process_id, .. } = target {
        // This background worker has no AppKit run loop to drain temporaries.
        // Reuse only the pure conversion; never publish a foreground Safari PID.
        Some(objc2::rc::autoreleasepool(|pool| {
            let app = NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)?;
            foreground_app_from_running_application(&app, pool)
        })?)
    } else {
        None
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

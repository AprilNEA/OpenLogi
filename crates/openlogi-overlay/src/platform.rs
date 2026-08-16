//! Native window policy for the standalone Actions Ring overlay.

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelativeStackTarget<T> {
    Keep,
    After(T),
    Top,
    Topmost,
}

#[cfg(any(target_os = "windows", test))]
fn choose_windows_stack_target<T: Copy + Eq>(
    overlay: T,
    foreground: Option<T>,
    predecessor: Option<T>,
    overlay_is_topmost: bool,
    foreground_is_topmost: bool,
    predecessor_is_topmost: bool,
) -> RelativeStackTarget<T> {
    let Some(foreground) = foreground.filter(|candidate| *candidate != overlay) else {
        return RelativeStackTarget::Top;
    };
    if predecessor == Some(overlay) && overlay_is_topmost == foreground_is_topmost {
        return RelativeStackTarget::Keep;
    }
    if foreground_is_topmost == predecessor_is_topmost {
        if let Some(predecessor) = predecessor.filter(|candidate| *candidate != foreground) {
            return RelativeStackTarget::After(predecessor);
        }
    }
    if foreground_is_topmost {
        RelativeStackTarget::Topmost
    } else {
        RelativeStackTarget::Top
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct MacWindowCandidate {
    number: isize,
    owner_pid: i32,
    layer: i32,
    bounds: core_graphics::geometry::CGRect,
}

#[cfg(target_os = "macos")]
fn select_frontmost_macos_window(
    frontmost_pid: i32,
    overlay_window_number: isize,
    cursor: Option<core_graphics::geometry::CGPoint>,
    candidates: impl IntoIterator<Item = MacWindowCandidate>,
) -> Option<isize> {
    let mut fallback = None;
    for candidate in candidates.into_iter().filter(|candidate| {
        candidate.owner_pid == frontmost_pid
            && candidate.number != overlay_window_number
            && candidate.layer == 0
    }) {
        fallback.get_or_insert(candidate.number);
        if cursor.is_some_and(|cursor| candidate.bounds.contains(&cursor)) {
            return Some(candidate.number);
        }
    }
    fallback
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreGraphics exposes CGWindow metadata keys as extern static CFStringRef values"
)]
fn frontmost_macos_window_number(
    overlay_window_number: isize,
    cursor: Option<core_graphics::geometry::CGPoint>,
) -> Option<isize> {
    use core_foundation::{dictionary::CFDictionary, number::CFNumber};
    use core_graphics::{
        geometry::CGRect,
        window::{
            create_description_from_array, create_window_list, kCGNullWindowID, kCGWindowBounds,
            kCGWindowLayer, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
            kCGWindowNumber, kCGWindowOwnerPID,
        },
    };
    use objc2_app_kit::NSWorkspace;

    fn number(
        dictionary: &core_foundation::dictionary::CFDictionary<
            core_foundation::string::CFString,
            core_foundation::base::CFType,
        >,
        key: core_foundation::string::CFStringRef,
    ) -> Option<i64> {
        dictionary.find(key)?.downcast::<CFNumber>()?.to_i64()
    }

    let frontmost_pid = NSWorkspace::sharedWorkspace()
        .frontmostApplication()?
        .processIdentifier();
    if frontmost_pid < 0 {
        return None;
    }

    let window_ids = create_window_list(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    let descriptions = create_description_from_array(window_ids)?;
    // SAFETY: these are immutable CoreGraphics metadata-key constants exported
    // by the framework and valid for the lifetime of the process.
    let (owner_pid_key, number_key, layer_key, bounds_key) = unsafe {
        (
            kCGWindowOwnerPID,
            kCGWindowNumber,
            kCGWindowLayer,
            kCGWindowBounds,
        )
    };
    let candidates = descriptions.iter().filter_map(|description| {
        let owner_pid = i32::try_from(number(&description, owner_pid_key)?).ok()?;
        let window_number = isize::try_from(number(&description, number_key)?).ok()?;
        let layer = i32::try_from(number(&description, layer_key)?).ok()?;
        let bounds = description.find(bounds_key)?.downcast::<CFDictionary>()?;
        let bounds = CGRect::from_dict_representation(&bounds)?;
        Some(MacWindowCandidate {
            number: window_number,
            owner_pid,
            layer,
            bounds,
        })
    });

    select_frontmost_macos_window(frontmost_pid, overlay_window_number, cursor, candidates)
}

/// Keep the overlay out of the Dock and app switcher.
#[cfg(target_os = "macos")]
pub fn configure_application() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(marker) = MainThreadMarker::new() {
        NSApplication::sharedApplication(marker)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Make the transparent ring panel borderless and remove its native shadow.
#[cfg(target_os = "macos")]
pub fn configure_windows() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowStyleMask};

    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker).windows() {
            window.setStyleMask(NSWindowStyleMask::NonactivatingPanel);
            window.setHasShadow(false);
        }
    }
}

/// No native application policy is required away from macOS.
#[cfg(not(target_os = "macos"))]
pub fn configure_application() {}

/// Other GPUI backends need no additional native window configuration here.
#[cfg(not(target_os = "macos"))]
pub fn configure_windows() {}

/// Owner of the native click-away event monitor; dropping it removes the
/// monitor. Create and drop on the main thread.
#[cfg(target_os = "macos")]
pub struct ClickAwayMonitor(objc2::rc::Retained<objc2::runtime::AnyObject>);

#[cfg(target_os = "macos")]
impl Drop for ClickAwayMonitor {
    #[expect(
        unsafe_code,
        reason = "NSEvent::removeMonitor is plain AppKit FFI; the token is exactly what addGlobalMonitor returned"
    )]
    fn drop(&mut self) {
        // SAFETY: `self.0` is the monitor token returned by
        // `addGlobalMonitorForEventsMatchingMask_handler`, removed only once.
        unsafe { objc2_app_kit::NSEvent::removeMonitor(&self.0) };
    }
}

/// Invoke `on_mouse_down` for every mouse-down that macOS delivers to *other*
/// applications, for as long as the returned monitor is held.
///
/// Global `NSEvent` monitors never see events routed to this process's own
/// windows and cannot consume the events they observe — together exactly the
/// ring's click-away contract: clicks on the ring keep hitting the GPUI
/// handlers they always did, while a click anywhere else can dismiss the ring
/// without being swallowed. Must be called on the main thread (returns `None`
/// off it); the handler runs on the main run loop.
#[cfg(target_os = "macos")]
pub fn watch_clicks_outside(on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask};

    MainThreadMarker::new()?;
    let handler: block2::RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>)> =
        block2::RcBlock::new(move |_event| on_mouse_down());
    NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &handler,
    )
    .map(ClickAwayMonitor)
}

/// Away from macOS no global click monitor is available; the ring keeps its
/// in-window dismissal paths (center ×, slot activation, timeout).
#[cfg(not(target_os = "macos"))]
pub struct ClickAwayMonitor(());

#[cfg(not(target_os = "macos"))]
pub fn watch_clicks_outside(_on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    None
}

/// One display's global geometry, in the same top-left-origin global point
/// space that `openlogi_hook::cursor_position()` reports.
pub struct CursorDisplay {
    /// Native display id; on macOS the `CGDirectDisplayID`, numerically equal
    /// to GPUI's `DisplayId` for the same display.
    pub id: u64,
    /// Global origin (top-left corner) of the display, in points.
    pub origin: (f64, f64),
    /// Display size in points.
    pub size: (f64, f64),
}

/// Find the display whose global bounds contain the point `(x, y)`.
///
/// GPUI's `PlatformDisplay::bounds()` zeroes every display's origin (window
/// bounds are display-relative), so mapping a global cursor position to its
/// display has to go through CoreGraphics.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CGGetActiveDisplayList/CGDisplayBounds are plain C FFI; GPUI exposes no global display bounds"
)]
pub fn display_containing(x: f64, y: f64) -> Option<CursorDisplay> {
    use core_graphics::display::{CGDisplayBounds, CGGetActiveDisplayList};

    const MAX_DISPLAYS: u32 = 32;
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: the list write is bounded by the capacity we pass; `count`
    // reports how many entries were actually filled.
    let result = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if result != 0 {
        return None;
    }
    ids.iter().take(count as usize).find_map(|&id| {
        // SAFETY: side-effect-free C getter on an id from the active list.
        let bounds = unsafe { CGDisplayBounds(id) };
        let contains = x >= bounds.origin.x
            && x < bounds.origin.x + bounds.size.width
            && y >= bounds.origin.y
            && y < bounds.origin.y + bounds.size.height;
        contains.then(|| CursorDisplay {
            id: u64::from(id),
            origin: (bounds.origin.x, bounds.origin.y),
            size: (bounds.size.width, bounds.size.height),
        })
    })
}

/// Away from macOS the GPUI display list already carries global origins, so
/// there is nothing to resolve natively.
#[cfg(not(target_os = "macos"))]
pub fn display_containing(_x: f64, _y: f64) -> Option<CursorDisplay> {
    None
}

/// Whether this native backend can keep one Actions Ring window allocated and
/// later hide/show/reposition it without rebuilding GPUI's renderer.
#[cfg(target_os = "windows")]
pub fn supports_warm_window(window: &gpui::Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window)
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Win32(_)))
}

#[cfg(target_os = "macos")]
pub const fn supports_warm_window(_window: &gpui::Window) -> bool {
    true
}

#[cfg(target_os = "linux")]
pub fn supports_warm_window(window: &gpui::Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window).is_ok_and(|handle| {
        matches!(
            handle.as_raw(),
            RawWindowHandle::Xcb(_) | RawWindowHandle::Xlib(_)
        )
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub const fn supports_warm_window(_window: &gpui::Window) -> bool {
    false
}

// `SetWindowRgn` consumes device-unit coordinates, so the Windows helper
// derives the circular region from the live HWND after DPI has settled.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "windows-sys 0.61.2 omits SetWindowRgn, so bind its documented User32 ABI locally"
)]
#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowRgn(
        hwnd: windows_sys::Win32::Foundation::HWND,
        region: windows_sys::Win32::Graphics::Gdi::HRGN,
        redraw: i32,
    ) -> i32;
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Win32 window regions are the native input/visibility shape primitive for a circular popup"
)]
pub fn apply_circular_shape(window: &gpui::Window) -> bool {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{CreateEllipticRgn, DeleteObject},
        UI::WindowsAndMessaging::GetWindowRect,
    };

    let Some(hwnd) = hwnd(window) else {
        return false;
    };
    let mut window_rect = RECT::default();
    // SAFETY: `window_rect` is a valid writable RECT and `hwnd` belongs to the
    // live GPUI window.
    if unsafe { GetWindowRect(hwnd, &raw mut window_rect) } == 0 {
        return false;
    }
    let width = (window_rect.right - window_rect.left).max(1);
    let height = (window_rect.bottom - window_rect.top).max(1);
    // SAFETY: CreateEllipticRgn only consumes integer bounds and returns an
    // owned GDI region handle on success.
    let region = unsafe { CreateEllipticRgn(0, 0, width, height) };
    if region.is_null() {
        return false;
    }
    // SAFETY: `hwnd` belongs to the live GPUI window. On success Windows
    // takes ownership of `region`; on failure ownership remains here.
    let applied = unsafe { SetWindowRgn(hwnd, region, 1) } != 0;
    if !applied {
        // SAFETY: SetWindowRgn failed, so the region is still caller-owned.
        unsafe { DeleteObject(region) };
    }
    applied
}

#[cfg(not(target_os = "windows"))]
pub const fn apply_circular_shape(_window: &gpui::Window) -> bool {
    false
}

/// Hide the warm Actions Ring window without destroying its renderer or GPUI
/// view. A hidden native window cannot steal pointer input.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "ShowWindow is the Win32 visibility primitive; HWND comes from GPUI's raw window handle"
)]
pub fn hide_window(window: &gpui::Window) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    let Some(hwnd) = hwnd(window) else {
        return false;
    };
    // SAFETY: `hwnd` belongs to the live GPUI window passed by reference.
    unsafe { ShowWindow(hwnd, SW_HIDE) };
    true
}

/// Add the native non-activating style without disturbing GPUI's other extended styles.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "the Win32 extended style is the native guarantee that the ring cannot take foreground focus"
)]
fn ensure_windows_no_activate(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE,
    };

    let Ok(no_activate_style) = isize::try_from(WS_EX_NOACTIVATE) else {
        return false;
    };
    // SAFETY: `hwnd` is the live GPUI overlay. Reading and rewriting the
    // extended style preserves every GPUI bit and only adds WS_EX_NOACTIVATE.
    let current_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if current_style & no_activate_style == 0 {
        // SAFETY: `hwnd` is live and the new value is the existing style with
        // the documented non-activating bit added.
        unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, current_style | no_activate_style) };
        // SAFETY: same live HWND; re-reading verifies that the style change
        // actually took effect before the popup can become interactive.
        let updated_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        if updated_style & no_activate_style == 0 {
            return false;
        }
    }
    true
}

/// Reposition the hidden HWND around the cursor after settling any cross-DPI
/// monitor transition. The caller keeps it hidden until the circular region is
/// rebuilt from the settled native size.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Win32 cursor/monitor/window placement is required because GPUI exposes no public move API for an existing window"
)]
fn position_windows_overlay_at_cursor(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        UI::WindowsAndMessaging::{
            GetCursorPos, GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
        },
    };

    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a valid writable POINT.
    if unsafe { GetCursorPos(&raw mut cursor) } == 0 {
        return false;
    }

    // SAFETY: `cursor` is a plain screen-space point.
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return false;
    }
    let Ok(cb_size) = u32::try_from(std::mem::size_of::<MONITORINFO>()) else {
        return false;
    };
    let mut monitor_info = MONITORINFO {
        cbSize: cb_size,
        ..Default::default()
    };
    // SAFETY: `monitor` is returned by MonitorFromPoint and the struct size is
    // initialized as required by GetMonitorInfoW.
    if unsafe { GetMonitorInfoW(monitor, &raw mut monitor_info) } == 0 {
        return false;
    }

    // Drop the old region before a possible DPI resize. The window is still
    // hidden, so this cannot flash a rectangular frame on screen.
    // SAFETY: a null HRGN removes the current region; `hwnd` is live.
    unsafe { SetWindowRgn(hwnd, std::ptr::null_mut(), 0) };

    // Stage the hidden window on the target monitor so GPUI can process a
    // synchronous WM_DPICHANGED before the authoritative native size is read.
    // SAFETY: the monitor origin is valid screen space and SWP_NOSIZE preserves
    // the existing renderer allocation during the staging move.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            monitor_info.rcMonitor.left,
            monitor_info.rcMonitor.top,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
    } == 0
    {
        return false;
    }

    let mut window_rect = RECT::default();
    // SAFETY: the staging move has settled any DPI-triggered native resize, so
    // this RECT is the authoritative device-pixel window size.
    if unsafe { GetWindowRect(hwnd, &raw mut window_rect) } == 0 {
        return false;
    }
    let width = (window_rect.right - window_rect.left).max(1);
    let height = (window_rect.bottom - window_rect.top).max(1);
    let desired_x = cursor.x - width / 2;
    let desired_y = cursor.y - height / 2;
    let max_x = (monitor_info.rcMonitor.right - width).max(monitor_info.rcMonitor.left);
    let max_y = (monitor_info.rcMonitor.bottom - height).max(monitor_info.rcMonitor.top);
    let x = desired_x.clamp(monitor_info.rcMonitor.left, max_x);
    let y = desired_y.clamp(monitor_info.rcMonitor.top, max_y);

    // SAFETY: x/y are clamped to the cursor's monitor and the window remains
    // hidden until its circular region has been rebuilt.
    let positioned = unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
    };
    positioned != 0
}

/// Reveal the shaped overlay immediately above the foreground window without
/// activating it or globally pinning it above unrelated windows.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Win32 exposes foreground-relative z-order only through HWND inspection and SetWindowPos"
)]
fn show_windows_above_foreground(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GW_HWNDPREV, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
        HWND_NOTOPMOST, HWND_TOPMOST, IsWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
        SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos, WS_EX_TOPMOST,
    };

    // SAFETY: GetForegroundWindow only reads the desktop foreground state.
    let foreground = unsafe { GetForegroundWindow() };
    let foreground = if foreground.is_null() || foreground == hwnd {
        None
    } else {
        // SAFETY: IsWindow only validates the HWND value returned above.
        let valid = unsafe { IsWindow(foreground) } != 0;
        valid.then_some(foreground)
    };
    let predecessor = foreground.and_then(|foreground| {
        // SAFETY: `foreground` was validated as a live top-level HWND.
        let predecessor = unsafe { GetWindow(foreground, GW_HWNDPREV) };
        (!predecessor.is_null()).then_some(predecessor)
    });
    let Ok(topmost_style) = isize::try_from(WS_EX_TOPMOST) else {
        return false;
    };
    let is_topmost = |candidate| {
        // SAFETY: candidates are the live overlay HWND or handles returned from
        // the validated foreground window's z-order chain.
        let ex_style = unsafe { GetWindowLongPtrW(candidate, GWL_EXSTYLE) };
        ex_style & topmost_style != 0
    };
    let overlay_is_topmost = is_topmost(hwnd);
    let foreground_is_topmost = foreground.is_some_and(is_topmost);
    let predecessor_is_topmost = predecessor.is_some_and(is_topmost);
    let stack_target = choose_windows_stack_target(
        hwnd,
        foreground,
        predecessor,
        overlay_is_topmost,
        foreground_is_topmost,
        predecessor_is_topmost,
    );
    let (insert_after, keep_z_order) = match stack_target {
        RelativeStackTarget::Keep => (std::ptr::null_mut(), true),
        RelativeStackTarget::After(predecessor) => (predecessor, false),
        RelativeStackTarget::Top => (HWND_NOTOPMOST, false),
        RelativeStackTarget::Topmost => (HWND_TOPMOST, false),
    };
    let mut flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOOWNERZORDER | SWP_NOSIZE | SWP_SHOWWINDOW;
    if keep_z_order {
        flags |= SWP_NOZORDER;
    }

    // SAFETY: the HWND is already positioned and shaped. SWP_NOACTIVATE keeps
    // keyboard focus in the current app while the insertion handle changes only
    // this reveal's relative z-order.
    let shown = unsafe { SetWindowPos(hwnd, insert_after, 0, 0, 0, 0, flags) };
    shown != 0
}

/// Reposition the already-created ring around the current cursor and reveal it
/// without activating it. The HWND remains warm between invocations.
#[cfg(target_os = "windows")]
pub fn show_window_at_cursor(window: &gpui::Window) -> bool {
    let Some(hwnd) = hwnd(window) else {
        return false;
    };
    ensure_windows_no_activate(hwnd)
        && position_windows_overlay_at_cursor(hwnd)
        && apply_circular_shape(window)
        && show_windows_above_foreground(hwnd)
}

#[cfg(target_os = "windows")]
fn hwnd(window: &gpui::Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::RawWindowHandle;

    let handle = raw_window_handle::HasWindowHandle::window_handle(window)
        .ok()?
        .as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as _),
        _ => None,
    }
}

/// AppKit keeps the GPUI NSWindow allocated while `orderOut` removes it from
/// the screen, so subsequent ring opens avoid NSWindow/Metal reconstruction.
#[cfg(target_os = "macos")]
pub fn hide_window(_window: &gpui::Window) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(marker) = MainThreadMarker::new() else {
        return false;
    };
    let windows = NSApplication::sharedApplication(marker).windows();
    let mut found = false;
    for window in windows {
        found = true;
        window.orderOut(None);
    }
    found
}

/// Move the retained NSWindow to the screen under the cursor and order it
/// immediately above the frontmost application's window without activating the
/// accessory overlay application.
#[cfg(target_os = "macos")]
pub fn show_window_at_cursor(_window: &gpui::Window) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSScreen, NSWindowOrderingMode};

    let Some(marker) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(marker);
    let Some(window) = app.windows().into_iter().next() else {
        return false;
    };
    let cursor = NSEvent::mouseLocation();
    let screens = NSScreen::screens(marker);
    let screen_frame = screens
        .into_iter()
        .find_map(|screen| {
            let frame = screen.frame();
            let inside = cursor.x >= frame.origin.x
                && cursor.x < frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y < frame.origin.y + frame.size.height;
            inside.then_some(frame)
        })
        .or_else(|| NSScreen::mainScreen(marker).map(|screen| screen.frame()));
    let Some(screen) = screen_frame else {
        return false;
    };

    let frame = window.frame();
    let max_x = (screen.origin.x + screen.size.width - frame.size.width).max(screen.origin.x);
    let max_y = (screen.origin.y + screen.size.height - frame.size.height).max(screen.origin.y);
    let mut origin = frame.origin;
    origin.x = (cursor.x - frame.size.width / 2.0).clamp(screen.origin.x, max_x);
    origin.y = (cursor.y - frame.size.height / 2.0).clamp(screen.origin.y, max_y);
    window.setFrameOrigin(origin);

    // CoreGraphics uses the same top-left global coordinate space as the
    // low-level hook cursor helper, unlike AppKit's bottom-left screen space.
    let cg_cursor = openlogi_hook::cursor_position()
        .map(|cursor| core_graphics::geometry::CGPoint::new(cursor.x, cursor.y));
    let target = frontmost_macos_window_number(window.windowNumber(), cg_cursor);
    if let Some(target) = target {
        window.orderWindow_relativeTo(NSWindowOrderingMode::Above, target);
    } else {
        window.orderFrontRegardless();
    }
    true
}

#[cfg(any(target_os = "linux", test))]
fn select_x11_active_sibling(overlay: u32, active_root_child: Option<u32>) -> Option<u32> {
    active_root_child.filter(|active| *active != 0 && *active != overlay)
}

#[cfg(target_os = "linux")]
fn x11_root_child_for_window(
    connection: &x11rb::rust_connection::RustConnection,
    root: u32,
    window: u32,
) -> Option<u32> {
    use x11rb::protocol::xproto::ConnectionExt as _;

    if window == 0 || window == root {
        return None;
    }
    let mut current = window;
    for _ in 0..32 {
        let tree = connection.query_tree(current).ok()?.reply().ok()?;
        if tree.root != root {
            return None;
        }
        if tree.parent == root {
            return Some(current);
        }
        if tree.parent == 0 || tree.parent == current {
            return None;
        }
        current = tree.parent;
    }
    None
}

#[cfg(target_os = "linux")]
thread_local! {
    static X11_CONNECTION: std::cell::RefCell<Option<x11rb::rust_connection::RustConnection>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "linux")]
fn with_x11_connection<R>(
    operation: impl FnOnce(&x11rb::rust_connection::RustConnection) -> Option<R>,
) -> Option<R> {
    X11_CONNECTION.with(|slot| {
        if slot.borrow().is_none() {
            let (connection, _) = x11rb::connect(None).ok()?;
            *slot.borrow_mut() = Some(connection);
        }
        let connection = slot.borrow();
        operation(connection.as_ref()?)
    })
}

#[cfg(target_os = "linux")]
fn x11_window_id(window: &gpui::Window) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
        _ => None,
    }
}

/// X11 can unmap an existing client window without destroying it. Wayland does
/// not expose equivalent global positioning/remapping semantics for this popup,
/// so Wayland deliberately stays on the existing create/destroy fallback.
#[cfg(target_os = "linux")]
pub fn hide_window(window: &gpui::Window) -> bool {
    use x11rb::{connection::Connection as _, protocol::xproto::ConnectionExt as _};

    let Some(window_id) = x11_window_id(window) else {
        return false;
    };
    with_x11_connection(|connection| {
        connection.unmap_window(window_id).ok()?;
        connection.flush().ok()?;
        Some(())
    })
    .is_some()
}

/// Reposition and map the retained X11 window around the current root-pointer,
/// stacking it immediately above the EWMH active window when that sibling is
/// valid on the same root. The cached connection avoids reconnecting on the hot
/// path and no focus request is sent.
#[cfg(target_os = "linux")]
pub fn show_window_at_cursor(window: &gpui::Window) -> bool {
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{AtomEnum, ConfigureWindowAux, ConnectionExt as _, StackMode},
    };

    let Some(window_id) = x11_window_id(window) else {
        return false;
    };
    with_x11_connection(|connection| {
        let geometry = connection.get_geometry(window_id).ok()?.reply().ok()?;
        let pointer = connection.query_pointer(geometry.root).ok()?.reply().ok()?;
        let screen = connection
            .setup()
            .roots
            .iter()
            .find(|screen| screen.root == geometry.root)?;

        let width = i32::from(geometry.width).max(1);
        let height = i32::from(geometry.height).max(1);
        let max_x = (i32::from(screen.width_in_pixels) - width).max(0);
        let max_y = (i32::from(screen.height_in_pixels) - height).max(0);
        let x = (i32::from(pointer.root_x) - width / 2).clamp(0, max_x);
        let y = (i32::from(pointer.root_y) - height / 2).clamp(0, max_y);

        let active_sibling = (|| {
            let active_atom = connection
                .intern_atom(false, b"_NET_ACTIVE_WINDOW")
                .ok()?
                .reply()
                .ok()?
                .atom;
            let active = connection
                .get_property(false, geometry.root, active_atom, AtomEnum::WINDOW, 0, 1)
                .ok()?
                .reply()
                .ok()?
                .value32()?
                .next()?;
            let active_root_child = x11_root_child_for_window(connection, geometry.root, active);
            select_x11_active_sibling(window_id, active_root_child)
        })();

        let mut configure = ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .stack_mode(StackMode::ABOVE);
        if let Some(active_sibling) = active_sibling {
            configure = configure.sibling(active_sibling);
        }
        connection.configure_window(window_id, &configure).ok()?;
        connection.map_window(window_id).ok()?;
        connection.flush().ok()?;
        Some(())
    })
    .is_some()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub const fn hide_window(_window: &gpui::Window) -> bool {
    false
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub const fn show_window_at_cursor(_window: &gpui::Window) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::{MacWindowCandidate, select_frontmost_macos_window};
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};

    fn candidate(number: isize, owner_pid: i32, x: f64) -> MacWindowCandidate {
        MacWindowCandidate {
            number,
            owner_pid,
            layer: 0,
            bounds: CGRect::new(&CGPoint::new(x, 0.0), &CGSize::new(100.0, 100.0)),
        }
    }

    #[test]
    fn macos_relative_stack_prefers_frontmost_app_window_under_cursor() {
        let candidates = [candidate(10, 7, 0.0), candidate(11, 7, 200.0)];
        assert_eq!(
            select_frontmost_macos_window(7, 99, Some(CGPoint::new(250.0, 50.0)), candidates),
            Some(11),
        );
    }

    #[test]
    fn macos_relative_stack_falls_back_to_frontmost_normal_window() {
        let candidates = [candidate(10, 7, 0.0), candidate(11, 8, 200.0)];
        assert_eq!(
            select_frontmost_macos_window(7, 99, None, candidates),
            Some(10),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{RelativeStackTarget, choose_windows_stack_target, select_x11_active_sibling};

    #[test]
    fn windows_relative_stack_uses_predecessor() {
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(10), Some(20), false, false, false),
            RelativeStackTarget::After(20),
        );
    }

    #[test]
    fn windows_relative_stack_does_not_cross_into_topmost_band() {
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(10), Some(20), false, false, true),
            RelativeStackTarget::Top,
        );
    }

    #[test]
    fn windows_relative_stack_keeps_existing_immediate_position() {
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(10), Some(99), false, false, false),
            RelativeStackTarget::Keep,
        );
    }

    #[test]
    fn windows_relative_stack_demotes_stale_topmost_overlay() {
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(10), Some(99), true, false, true),
            RelativeStackTarget::Top,
        );
    }

    #[test]
    fn windows_relative_stack_falls_back_to_normal_top() {
        assert_eq!(
            choose_windows_stack_target(99_u32, None, None, false, false, false),
            RelativeStackTarget::Top,
        );
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(99), None, false, false, false),
            RelativeStackTarget::Top,
        );
    }

    #[test]
    fn windows_relative_stack_preserves_topmost_band_only_when_required() {
        assert_eq!(
            choose_windows_stack_target(99_u32, Some(10), None, true, true, false),
            RelativeStackTarget::Topmost,
        );
    }

    #[test]
    fn x11_relative_stack_uses_active_window_on_same_root() {
        assert_eq!(select_x11_active_sibling(99, Some(10)), Some(10),);
    }

    #[test]
    fn x11_relative_stack_rejects_missing_self_and_other_root_targets() {
        assert_eq!(select_x11_active_sibling(99, None), None);
        assert_eq!(select_x11_active_sibling(99, Some(99)), None);
        assert_eq!(select_x11_active_sibling(99, Some(0)), None);
    }
}

//! The Win32 boundary: sample once, establish the HWND's target DPI while it
//! is still a 1px bootstrap, then place the window without activating it, so
//! that no later click activates it either. The ring and the notice share
//! both halves; only their final geometry differs.

use anyhow::{Context as _, Result, ensure};
use gpui::{App, Bounds, DevicePixels, DisplayId, Pixels, Point, Size, Window, point, size};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use tracing::{debug, warn};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetCursorPos, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowLongW, SetWindowPos, WS_CAPTION, WS_EX_NOACTIVATE,
    WS_POPUP, WS_THICKFRAME,
};

use super::{NoticePlacement, RingPlacement};

impl RingPlacement {
    pub(crate) fn capture(_cx: &mut App) -> Result<Self> {
        let sample = sample_monitor()?;
        Ok(Self {
            display_id: sample.display_id,
            cursor: sample.cursor,
            display: sample.display,
            dpi: sample.dpi,
        })
    }

    pub(crate) fn show(self, window: &mut Window) -> Result<()> {
        place(window, self.display_id, self.display, |dpi| {
            self.bounds(dpi)
        })
    }
}

impl NoticePlacement {
    pub(crate) fn capture(_cx: &mut App, size: Size<Pixels>) -> Result<Self> {
        let sample = sample_monitor()?;
        Ok(Self {
            display_id: sample.display_id,
            display: sample.display,
            work: sample.work,
            dpi: sample.dpi,
            size,
        })
    }

    pub(crate) fn show(self, window: &mut Window) -> Result<()> {
        place(window, self.display_id, self.display, |dpi| {
            self.bounds(dpi)
        })
    }
}

/// One reading of the cursor and the monitor under it.
struct MonitorSample {
    display_id: DisplayId,
    /// The cursor, or the monitor's centre when it could not be read.
    cursor: Point<DevicePixels>,
    /// The whole monitor (`rcMonitor`).
    display: Bounds<DevicePixels>,
    /// The monitor without the taskbar (`rcWork`).
    work: Bounds<DevicePixels>,
    dpi: u32,
}

#[expect(
    unsafe_code,
    reason = "read-only Win32 cursor and monitor queries with checked outputs"
)]
fn sample_monitor() -> Result<MonitorSample> {
    let mut cursor = POINT::default();
    // SAFETY: cursor is writable for the duration of the call. This GPUI
    // process is PerMonitorV2-aware, so GetCursorPos returns physical pixels.
    let has_cursor = unsafe { GetCursorPos(&raw mut cursor) } != 0;
    let monitor = if has_cursor {
        // SAFETY: POINT is passed by value. Use this same sample for both
        // selection and positioning, never a second cursor read.
        unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) }
    } else {
        // SAFETY: a null HWND plus DEFAULTTOPRIMARY selects the primary.
        unsafe { MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY) }
    };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>())?,
        ..MONITORINFO::default()
    };
    // SAFETY: monitor is from a native query and info has the required size;
    // a removed monitor is reported as failure, not used as geometry.
    if unsafe { GetMonitorInfoW(monitor, &raw mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query overlay monitor bounds");
    }
    let display = device_bounds(info.rcMonitor);
    let (mut dpi, mut dpi_y) = (0, 0);
    // SAFETY: monitor is from a native query and both outputs are writable
    // for the duration of the call; a failed HRESULT is not used as a DPI.
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) };
    ensure!(
        hr >= 0 && dpi != 0,
        "could not read overlay monitor DPI (HRESULT {hr:#x})"
    );
    Ok(MonitorSample {
        // The pinned GPUI Windows backend uses HMONITOR as DisplayId.
        display_id: DisplayId::from(monitor as usize as u64),
        cursor: if has_cursor {
            point(DevicePixels(cursor.x), DevicePixels(cursor.y))
        } else {
            display.center()
        },
        display,
        work: device_bounds(info.rcWork),
        dpi,
    })
}

fn device_bounds(rect: RECT) -> Bounds<DevicePixels> {
    Bounds::new(
        point(DevicePixels(rect.left), DevicePixels(rect.top)),
        size(
            DevicePixels(rect.right - rect.left),
            DevicePixels(rect.bottom - rect.top),
        ),
    )
}

/// Move a bootstrap window onto its monitor, then to `bounds` at the DPI it
/// ends up with.
#[expect(
    unsafe_code,
    reason = "borrow the live GPUI HWND on its owning UI thread"
)]
fn place(
    window: &mut Window,
    display_id: DisplayId,
    display: Bounds<DevicePixels>,
    bounds: impl FnOnce(u32) -> Bounds<DevicePixels>,
) -> Result<()> {
    let RawWindowHandle::Win32(handle) = window
        .window_handle()
        .map_err(|error| anyhow::anyhow!("get overlay HWND: {error}"))?
        .as_raw()
    else {
        anyhow::bail!("overlay window has no Win32 handle");
    };
    let hwnd = handle.hwnd.get() as HWND;
    // The frame is cosmetic: a framed overlay is better than none, so a
    // failure here must not abort placement.
    if let Err(error) = make_frameless(hwnd) {
        warn!(%error, "could not remove the overlay window frame");
    }
    // Without this, the click that picks a ring slot takes the foreground
    // from the application the slot's shortcut is meant for. The ring still
    // opens if it fails: actions that press no keys keep working.
    if let Err(error) = make_nonactivating(hwnd) {
        warn!(%error, "could not keep the overlay window from taking the foreground");
    }
    // Pin the 1px bootstrap well inside the selected monitor (GPUI should
    // already have opened it there). If that crosses a DPI boundary,
    // SetWindowPos synchronously delivers WM_DPICHANGED; GPUI applies its
    // suggested rectangle and updates rendering scale before this returns.
    // Final geometry is applied only AFTER that transition, so the suggested
    // rectangle cannot move the window away from where it was placed.
    // FRAMECHANGED applies the styles set above in the same move.
    let anchor = Bounds::new(display.center(), size(DevicePixels(1), DevicePixels(1)));
    set_bounds(
        hwnd,
        anchor,
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
    )?;
    // SAFETY: hwnd belongs to the still-borrowed window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    ensure!(dpi != 0, "could not read overlay window DPI");
    // SAFETY: read-only query on the still-borrowed window.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    ensure!(
        monitor as usize as u64 == u64::from(display_id),
        "overlay monitor changed during window creation"
    );
    set_bounds(
        hwnd,
        bounds(dpi),
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
    )
}

/// Strip the frame GPUI leaves on a pop-up window, so only the overlay's own
/// transparent pixels are drawn.
///
/// GPUI creates `WindowKind::PopUp` with a zero window style, which is
/// `WS_OVERLAPPED`, and `CreateWindowExW` gives every overlapped window a
/// caption. Windows 11 then draws that frame — an opaque backdrop, a border
/// and rounded corners — around the transparent client area. The window
/// becomes a borderless `WS_POPUP` instead; the caller's next `SetWindowPos`
/// must pass `SWP_FRAMECHANGED` to apply it. The DWM attributes keep Windows 11
/// from rounding or outlining it.
#[expect(
    unsafe_code,
    reason = "window-style and DWM attribute writes on a borrowed HWND, with typed values"
)]
fn make_frameless(hwnd: HWND) -> Result<()> {
    // GWL_STYLE is a 32-bit bit set, so the non-Ptr pair is right on every
    // target, and the casts reinterpret it without changing bits.
    // SAFETY: hwnd belongs to the still-borrowed window.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) }.cast_unsigned();
    let frameless = (style & !(WS_CAPTION | WS_THICKFRAME)) | WS_POPUP;
    // SAFETY: as above; a zero return is only an error with a last error set,
    // and the previous style is never zero for this captioned window.
    if unsafe { SetWindowLongW(hwnd, GWL_STYLE, frameless.cast_signed()) } == 0 {
        return Err(std::io::Error::last_os_error()).context("make overlay window frameless");
    }
    let size = u32::try_from(std::mem::size_of::<u32>())?;
    for (attribute, value) in [
        (
            DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND.cast_unsigned(),
        ),
        (DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE),
    ] {
        // SAFETY: value outlives this synchronous call, and `size` is the
        // DWORD both attributes take.
        let hr = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute.cast_unsigned(),
                std::ptr::from_ref(&value).cast(),
                size,
            )
        };
        // Windows 10 has neither attribute, and draws neither effect.
        if hr < 0 {
            debug!(attribute, "DWM attribute unsupported (HRESULT {hr:#x})");
        }
    }
    Ok(())
}

/// Keep a click on the overlay from taking the foreground away from the
/// application it opened over.
///
/// A ring action presses its shortcut into the window with keyboard focus,
/// so the application under the ring has to keep that focus through the
/// click that picks a slot, as it does under the macOS non-activating panel
/// and the Wayland layer surface without keyboard interactivity. GPUI shows
/// the window without activating it, but answers every `WM_MOUSEACTIVATE`
/// with `MA_ACTIVATE`, so that click made the overlay the foreground window
/// and the shortcut reached the closing overlay instead. Windows does not
/// activate a `WS_EX_NOACTIVATE` window when it is clicked. Active window
/// tracking ("activate a window by hovering over it") still asks
/// `WM_MOUSEACTIVATE`, so with that setting on, hovering can still activate it.
#[expect(
    unsafe_code,
    reason = "extended-style read and write on a borrowed HWND, with typed values"
)]
fn make_nonactivating(hwnd: HWND) -> Result<()> {
    // GWL_EXSTYLE is a 32-bit bit set like GWL_STYLE in `make_frameless`.
    // SAFETY: hwnd belongs to the still-borrowed window.
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned();
    let nonactivating = ex_style | WS_EX_NOACTIVATE;
    // SAFETY: as above; a zero return is only an error with a last error set,
    // and GPUI never creates a pop-up with a zero extended style.
    if unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, nonactivating.cast_signed()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("keep the overlay window from taking the foreground");
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "SetWindowPos on a borrowed HWND, with physical screen bounds"
)]
fn set_bounds(hwnd: HWND, bounds: Bounds<DevicePixels>, flags: u32) -> Result<()> {
    // SAFETY: the caller holds the live GPUI window on its owning thread;
    // NOZORDER makes the null insert-after handle irrelevant.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            bounds.origin.x.0,
            bounds.origin.y.0,
            bounds.size.width.0,
            bounds.size.height.0,
            flags,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("position overlay window");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    use super::*;

    /// A bare Win32 pop-up with the extended style GPUI gives a
    /// `WindowKind::PopUp`, destroyed on drop.
    struct PopUp(HWND);

    impl PopUp {
        const GPUI_EX_STYLE: u32 = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;

        #[expect(
            unsafe_code,
            reason = "create a plain system-class window for the test"
        )]
        fn open() -> Self {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: `class` is a NUL-terminated system class name that
            // outlives the call; every handle argument may be null.
            let hwnd = unsafe {
                CreateWindowExW(
                    Self::GPUI_EX_STYLE,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(
                !hwnd.is_null(),
                "create a test pop-up: {}",
                std::io::Error::last_os_error()
            );
            Self(hwnd)
        }

        #[expect(
            unsafe_code,
            reason = "read the extended style of the test's own window"
        )]
        fn ex_style(&self) -> u32 {
            // SAFETY: `self.0` is a live window this thread created.
            unsafe { GetWindowLongW(self.0, GWL_EXSTYLE) }.cast_unsigned()
        }
    }

    impl Drop for PopUp {
        #[expect(unsafe_code, reason = "destroy the test's own window")]
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live window this thread created, and it is
            // destroyed only here.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_click_cannot_activate_an_overlay_window() {
        let window = PopUp::open();
        assert_eq!(
            window.ex_style() & WS_EX_NOACTIVATE,
            0,
            "a bare pop-up is activated by a click, as GPUI's was"
        );
        make_nonactivating(window.0).expect("set the overlay's extended style");
        let ex_style = window.ex_style();
        assert_ne!(
            ex_style & WS_EX_NOACTIVATE,
            0,
            "a click must leave the foreground with the application under the overlay"
        );
        assert_eq!(
            ex_style & PopUp::GPUI_EX_STYLE,
            PopUp::GPUI_EX_STYLE,
            "the overlay stays a topmost tool window"
        );
    }
}

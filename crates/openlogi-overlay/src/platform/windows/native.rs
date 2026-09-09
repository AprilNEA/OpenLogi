//! The Win32 boundary: sample once, establish the HWND's target DPI while
//! hidden, then place and reveal the ring without activating it.

use anyhow::{Context as _, Result, ensure};
use gpui::{App, Bounds, DevicePixels, DisplayId, Window, point, size};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos,
};

use super::RingPlacement;

impl RingPlacement {
    #[expect(
        unsafe_code,
        reason = "read-only Win32 cursor and monitor queries with checked outputs"
    )]
    pub(crate) fn capture(_cx: &mut App) -> Result<Self> {
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
            return Err(std::io::Error::last_os_error()).context("query ring monitor bounds");
        }
        let rect = info.rcMonitor;
        let display = Bounds::new(
            point(DevicePixels(rect.left), DevicePixels(rect.top)),
            size(
                DevicePixels(rect.right - rect.left),
                DevicePixels(rect.bottom - rect.top),
            ),
        );
        Ok(Self {
            // The pinned GPUI Windows backend uses HMONITOR as DisplayId.
            display_id: DisplayId::from(monitor as usize as u64),
            cursor: if has_cursor {
                point(DevicePixels(cursor.x), DevicePixels(cursor.y))
            } else {
                display.center()
            },
            display,
        })
    }

    #[expect(
        unsafe_code,
        reason = "borrow the live GPUI HWND on its owning UI thread"
    )]
    pub(crate) fn show(self, window: &mut Window) -> Result<()> {
        let RawWindowHandle::Win32(handle) = window
            .window_handle()
            .map_err(|error| anyhow::anyhow!("get ring HWND: {error}"))?
            .as_raw()
        else {
            anyhow::bail!("ring window has no Win32 handle");
        };
        let hwnd = handle.hwnd.get() as HWND;
        // Bootstrap a tiny hidden window well inside the selected monitor.
        // SetWindowPos synchronously delivers WM_DPICHANGED; GPUI applies its
        // suggested rectangle and updates rendering scale before this returns.
        // Final geometry is applied only AFTER that transition, so the suggested
        // rectangle cannot move a cursor-centred ring away from its anchor.
        let anchor = Bounds::new(
            self.display.center(),
            size(DevicePixels(1), DevicePixels(1)),
        );
        set_bounds(hwnd, anchor, SWP_NOACTIVATE | SWP_NOZORDER)?;
        // SAFETY: hwnd belongs to the still-borrowed window.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        ensure!(dpi != 0, "could not read ring window DPI");
        // SAFETY: read-only query on the still-borrowed window.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        ensure!(
            monitor as usize as u64 == u64::from(self.display_id),
            "ring monitor changed during window creation"
        );
        set_bounds(
            hwnd,
            self.bounds(dpi),
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
        )
    }
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
        return Err(std::io::Error::last_os_error()).context("position Actions Ring window");
    }
    Ok(())
}

//! The Win32 boundary: sample once, establish the HWND's target DPI while it
//! is still a 1px bootstrap, then place the window without activating it.
//! The ring and the notice share both halves; only their final geometry
//! differs.

use anyhow::{Context as _, Result, ensure};
use gpui::{App, Bounds, DevicePixels, DisplayId, Pixels, Point, Size, Window, point, size};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos,
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
    // Pin the 1px bootstrap well inside the selected monitor (GPUI should
    // already have opened it there). If that crosses a DPI boundary,
    // SetWindowPos synchronously delivers WM_DPICHANGED; GPUI applies its
    // suggested rectangle and updates rendering scale before this returns.
    // Final geometry is applied only AFTER that transition, so the suggested
    // rectangle cannot move the window away from where it was placed.
    let anchor = Bounds::new(display.center(), size(DevicePixels(1), DevicePixels(1)));
    set_bounds(hwnd, anchor, SWP_NOACTIVATE | SWP_NOZORDER)?;
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

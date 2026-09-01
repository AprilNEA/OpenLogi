//! Windows native window policy and DPI-aware display lookup.

use super::{CursorPlacement, scaled_cursor_placement};

pub(super) const fn configure_application() {}

pub(super) fn configure_window(window: &gpui::Window) {
    use raw_window_handle::RawWindowHandle;
    use tracing::debug;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    };

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;

    for (name, result) in [
        (
            "non-client rendering",
            set_dwm_window_attribute(hwnd, DWMWA_NCRENDERING_POLICY, &DWMNCRP_DISABLED),
        ),
        (
            "border color",
            set_dwm_window_attribute(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE),
        ),
        (
            "corner preference",
            set_dwm_window_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_DONOTROUND),
        ),
    ] {
        if result.is_some_and(|result| result < 0) {
            debug!(
                result,
                attribute = name,
                "DWM window attribute is unavailable"
            );
        }
    }
}

#[expect(
    unsafe_code,
    reason = "DwmSetWindowAttribute synchronously copies the typed value for the live GPUI HWND"
)]
fn set_dwm_window_attribute<T>(
    hwnd: windows_sys::Win32::Foundation::HWND,
    attribute: i32,
    value: &T,
) -> Option<windows_sys::core::HRESULT> {
    use std::mem::size_of_val;

    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    let attribute = u32::try_from(attribute).ok()?;
    let value_size = u32::try_from(size_of_val(value)).ok()?;
    // SAFETY: `hwnd` belongs to the live GPUI window, `value` remains valid
    // for this synchronous call, and `value_size` is the exact pointee size.
    Some(unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            std::ptr::from_ref(value).cast(),
            value_size,
        )
    })
}

pub(crate) struct ClickAwayMonitor;

pub(super) fn watch_clicks_outside(
    _on_mouse_down: impl Fn() + 'static,
) -> Option<ClickAwayMonitor> {
    None
}

#[expect(
    unsafe_code,
    reason = "MonitorFromPoint/GetMonitorInfoW/GetDpiForMonitor are plain Win32 FFI"
)]
pub(super) fn cursor_placement(x: f64, y: f64) -> Option<CursorPlacement> {
    use std::mem::size_of;

    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    let point = win32_point(x, y)?;
    // SAFETY: `point` is an initialized screen coordinate and the null flag
    // asks Windows to return a null handle rather than selecting a nearby
    // monitor if the coordinate is not covered by a display.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return None;
    }

    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).ok()?,
        ..MONITORINFO::default()
    };
    // SAFETY: `monitor` is the non-null handle returned above and `info` is a
    // live MONITORINFO with its required size initialized.
    if unsafe { GetMonitorInfoW(monitor, &raw mut info) } == 0 {
        return None;
    }

    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    // SAFETY: `monitor` is valid for the duration of this synchronous query;
    // both DPI outputs point to live initialized u32 storage.
    let dpi_result =
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) };
    if dpi_result != 0 || dpi_x == 0 || dpi_x != dpi_y {
        return None;
    }

    let rect = info.rcMonitor;
    let display_origin = (f64::from(rect.left), f64::from(rect.top));
    let display_size = (
        f64::from(rect.right) - f64::from(rect.left),
        f64::from(rect.bottom) - f64::from(rect.top),
    );
    let display_id = u64::try_from(monitor.addr()).ok()?;
    scaled_cursor_placement(
        display_id,
        (x, y),
        display_origin,
        display_size,
        f64::from(dpi_x) / 96.0,
    )
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "cursor_position preserves Win32 i32 coordinates as f64 and the range checks prove they fit back into POINT"
)]
fn win32_point(x: f64, y: f64) -> Option<windows_sys::Win32::Foundation::POINT> {
    if !x.is_finite()
        || !y.is_finite()
        || x < f64::from(i32::MIN)
        || x > f64::from(i32::MAX)
        || y < f64::from(i32::MIN)
        || y > f64::from(i32::MAX)
    {
        return None;
    }
    Some(windows_sys::Win32::Foundation::POINT {
        x: x as i32,
        y: y as i32,
    })
}

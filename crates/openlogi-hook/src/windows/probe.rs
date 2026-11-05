//! Flow's pointer and display queries on Windows.
//!
//! The desktop space is the virtual-screen coordinates that `GetCursorPos`,
//! `GetMonitorInfoW` and `SetCursorPos` use for this thread. The three calls
//! agree with each other in every DPI-awareness mode, so no per-monitor scale
//! is applied. Unverified on a mixed-DPI desktop.
#![expect(unsafe_code, reason = "Win32 cursor and monitor queries")]

use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
use windows_sys::core::BOOL;

use super::hook::key_held;
use crate::{DesktopPoint, DisplayBounds, KeyModifiers, PointerState, WarpError};

/// The Windows probe. Win32 needs no connection, so it holds nothing.
pub(crate) struct Probe;

impl Probe {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the probe API is the same on every platform; X11 can fail to connect"
    )]
    pub(crate) fn connect() -> Option<Self> {
        Some(Self)
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn state(&mut self) -> Option<PointerState> {
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: `point` is a live, writable `POINT` for the whole call.
        if unsafe { GetCursorPos(&raw mut point) } == 0 {
            return None;
        }
        Some(PointerState {
            position: DesktopPoint {
                x: f64::from(point.x),
                y: f64::from(point.y),
            },
            modifiers: KeyModifiers {
                shift: key_held(VK_SHIFT),
                control: key_held(VK_CONTROL),
                option: key_held(VK_MENU),
                command: key_held(VK_LWIN) || key_held(VK_RWIN),
            },
        })
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn displays(&mut self) -> Vec<DisplayBounds> {
        let mut displays: Vec<DisplayBounds> = Vec::new();
        // SAFETY: `collect_monitor` runs synchronously inside this call, and
        // `displays` outlives it; the callback is the only user of the pointer.
        let listed = unsafe {
            EnumDisplayMonitors(
                ptr::null_mut(),
                ptr::null(),
                Some(collect_monitor),
                (&raw mut displays) as LPARAM,
            )
        };
        if listed == 0 {
            return Vec::new();
        }
        displays
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn warp(&mut self, to: DesktopPoint) -> Result<(), WarpError> {
        // SAFETY: `SetCursorPos` takes both coordinates by value.
        if unsafe { SetCursorPos(screen_pixel(to.x), screen_pixel(to.y)) } == 0 {
            return Err(WarpError::Rejected(io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// `EnumDisplayMonitors` callback: append one monitor's bounds to the
/// `Vec<DisplayBounds>` that `data` points to. A monitor that disappears
/// during the enumeration is skipped.
unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&raw mut Vec<DisplayBounds>` that `displays`
    // passed; that vector is alive and not otherwise borrowed during the call.
    let displays = unsafe { &mut *(data as *mut Vec<DisplayBounds>) };
    let Ok(size) = u32::try_from(size_of::<MONITORINFO>()) else {
        return TRUE;
    };
    let mut info = MONITORINFO {
        cbSize: size,
        ..MONITORINFO::default()
    };
    // SAFETY: `monitor` comes from the enumeration and `info` has `cbSize` set.
    if unsafe { GetMonitorInfoW(monitor, &raw mut info) } != 0 {
        displays.push(bounds(info.rcMonitor));
    }
    TRUE
}

fn bounds(rect: RECT) -> DisplayBounds {
    DisplayBounds {
        x: f64::from(rect.left),
        y: f64::from(rect.top),
        width: f64::from(rect.right) - f64::from(rect.left),
        height: f64::from(rect.bottom) - f64::from(rect.top),
    }
}

/// The nearest virtual-screen pixel, which Win32 carries as an `i32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the rounded value is clamped to the i32 range before conversion"
)]
fn screen_pixel(coordinate: f64) -> i32 {
    coordinate
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::RECT;

    use super::{bounds, screen_pixel};
    use crate::DisplayBounds;

    #[test]
    fn a_monitor_left_of_the_primary_keeps_its_negative_origin() {
        let rect = RECT {
            left: -2560,
            top: -200,
            right: 0,
            bottom: 1240,
        };

        assert_eq!(
            bounds(rect),
            DisplayBounds {
                x: -2560.0,
                y: -200.0,
                width: 2560.0,
                height: 1440.0,
            }
        );
    }

    #[test]
    fn screen_pixels_round_and_saturate() {
        assert_eq!(screen_pixel(-1279.5), -1280);
        assert_eq!(screen_pixel(1e12), i32::MAX);
        assert_eq!(screen_pixel(f64::NAN), 0);
    }
}

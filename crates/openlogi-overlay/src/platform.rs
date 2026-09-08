//! Native window policy and cursor geometry for the Actions Ring overlay.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod other;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use other as imp;
#[cfg(target_os = "windows")]
use windows as imp;

pub(crate) use imp::ClickAwayMonitor;

/// Apply the platform's process-wide overlay application policy.
pub(crate) fn configure_application() {
    imp::configure_application();
}

/// Apply native policy to the newly created transparent ring window.
pub(crate) fn configure_window(window: &gpui::Window) {
    imp::configure_window(window);
}

/// Watch mouse-down events that land outside this process's windows.
pub(crate) fn watch_clicks_outside(on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    imp::watch_clicks_outside(on_mouse_down)
}

/// A cursor and its display expressed in the coordinate space GPUI expects for
/// `WindowOptions`.
///
/// Windows uses global logical coordinates because GPUI's Windows backend
/// scales both the display bounds and the window bounds from that space. macOS
/// uses display-relative coordinates because its GPUI display bounds are
/// display-relative. Linux does not construct this type: its existing GPUI
/// fallback remains responsible for the global-coordinate mapping.
pub(crate) struct CursorPlacement {
    /// Native display id, numerically equal to the GPUI `DisplayId`.
    pub(crate) display_id: u64,
    /// Cursor position in GPUI logical coordinates.
    pub(crate) center: (f64, f64),
    /// Display bounds origin in GPUI logical coordinates.
    pub(crate) display_origin: (f64, f64),
    /// Display bounds size in GPUI logical coordinates.
    pub(crate) display_size: (f64, f64),
}

/// Resolve the native cursor placement for the current platform.
pub(crate) fn cursor_placement(x: f64, y: f64) -> Option<CursorPlacement> {
    imp::cursor_placement(x, y)
}

/// Whether a failed native lookup must fall back to the primary display.
///
/// Windows and macOS hook coordinates are not safe to feed directly into GPUI
/// when their native display lookup fails. Linux retains its existing
/// global-coordinate fallback.
pub(crate) const fn native_cursor_placement_requires_primary_fallback() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// Convert a physical global cursor/display geometry pair into GPUI's global
/// logical coordinate space.
///
/// Kept independent of Win32 so scale and negative-origin cases can be tested
/// on every host platform.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn scaled_cursor_placement(
    display_id: u64,
    cursor: (f64, f64),
    display_origin: (f64, f64),
    display_size: (f64, f64),
    scale_factor: f64,
) -> Option<CursorPlacement> {
    if !valid_geometry(cursor, display_origin, display_size)
        || !scale_factor.is_finite()
        || scale_factor <= 0.0
    {
        return None;
    }

    Some(CursorPlacement {
        display_id,
        center: (cursor.0 / scale_factor, cursor.1 / scale_factor),
        display_origin: (
            display_origin.0 / scale_factor,
            display_origin.1 / scale_factor,
        ),
        display_size: (display_size.0 / scale_factor, display_size.1 / scale_factor),
    })
}

#[cfg(any(target_os = "macos", test))]
fn relative_cursor_placement(
    display_id: u64,
    cursor: (f64, f64),
    display_origin: (f64, f64),
    display_size: (f64, f64),
) -> Option<CursorPlacement> {
    if !valid_geometry(cursor, display_origin, display_size) {
        return None;
    }

    Some(CursorPlacement {
        display_id,
        center: (cursor.0 - display_origin.0, cursor.1 - display_origin.1),
        display_origin: (0.0, 0.0),
        display_size,
    })
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn valid_geometry(
    cursor: (f64, f64),
    display_origin: (f64, f64),
    display_size: (f64, f64),
) -> bool {
    cursor.0.is_finite()
        && cursor.1.is_finite()
        && display_origin.0.is_finite()
        && display_origin.1.is_finite()
        && display_size.0.is_finite()
        && display_size.1.is_finite()
        && display_size.0 > 0.0
        && display_size.1 > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_at_100_percent_is_an_identity_conversion() {
        let placement =
            scaled_cursor_placement(7, (2560.0, 1080.0), (0.0, 0.0), (3840.0, 2160.0), 1.0)
                .expect("valid display geometry");

        assert_eq!(placement.display_id, 7);
        assert_eq!(placement.center, (2560.0, 1080.0));
        assert_eq!(placement.display_origin, (0.0, 0.0));
        assert_eq!(placement.display_size, (3840.0, 2160.0));
    }

    #[test]
    fn scaling_125_percent_converts_cursor_and_display_geometry() {
        let placement =
            scaled_cursor_placement(11, (2560.0, 1080.0), (0.0, 0.0), (5120.0, 2160.0), 1.25)
                .expect("valid display geometry");

        assert_eq!(placement.center, (2048.0, 864.0));
        assert_eq!(placement.display_origin, (0.0, 0.0));
        assert_eq!(placement.display_size, (4096.0, 1728.0));
    }

    #[test]
    fn scaling_150_percent_preserves_negative_monitor_origins() {
        let placement =
            scaled_cursor_placement(13, (-1920.0, 810.0), (-3840.0, 0.0), (3840.0, 2160.0), 1.5)
                .expect("valid display geometry");

        assert_eq!(placement.center, (-1280.0, 540.0));
        assert_eq!(placement.display_origin, (-2560.0, 0.0));
        assert_eq!(placement.display_size, (2560.0, 1440.0));
    }

    #[test]
    fn relative_placement_keeps_macos_display_coordinates() {
        let placement =
            relative_cursor_placement(17, (3000.0, 500.0), (2560.0, 0.0), (2560.0, 1440.0))
                .expect("valid display geometry");

        assert_eq!(placement.center, (440.0, 500.0));
        assert_eq!(placement.display_origin, (0.0, 0.0));
        assert_eq!(placement.display_size, (2560.0, 1440.0));
    }

    #[test]
    fn invalid_native_geometry_returns_none_for_primary_fallback() {
        assert!(
            scaled_cursor_placement(19, (100.0, 100.0), (0.0, 0.0), (1920.0, 1080.0), 0.0,)
                .is_none()
        );
    }
}

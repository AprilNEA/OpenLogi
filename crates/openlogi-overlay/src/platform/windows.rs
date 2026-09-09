//! Windows placement stays in physical virtual-desktop coordinates. GPUI's
//! per-monitor logical rectangles can overlap at mixed DPI, so neither cursor
//! monitor selection nor HWND positioning can use logical containment.

use gpui::{Bounds, DevicePixels, DisplayId, Point, WindowOptions, point, size};

use crate::ring::{WINDOW_SIZE, ring_window_options};

#[cfg(target_os = "windows")]
mod native;

/// One native cursor/monitor snapshot, kept through hidden-window creation.
pub(crate) struct RingPlacement {
    display_id: DisplayId,
    cursor: Point<DevicePixels>,
    display: Bounds<DevicePixels>,
}

impl RingPlacement {
    pub(crate) fn window_options(&self) -> WindowOptions {
        WindowOptions {
            display_id: Some(self.display_id),
            // GPUI initially converts bounds using the default-position HWND's
            // DPI, not the requested monitor's. Do not show that placement.
            show: false,
            ..ring_window_options()
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the ring's DIP edge scaled by a native display DPI fits in screen-sized i32 pixels"
    )]
    fn bounds(&self, dpi: u32) -> Bounds<DevicePixels> {
        let edge = DevicePixels((f64::from(WINDOW_SIZE) * f64::from(dpi) / 96.0).round() as i32);
        let half = DevicePixels(edge.0 / 2);
        let desired = self.cursor - point(half, half);
        // If the display is smaller than the ring, anchor to its top-left
        // rather than inverting the clamp range or changing the ring's scale.
        let max = point(
            (self.display.right() - edge).max(self.display.left()),
            (self.display.bottom() - edge).max(self.display.top()),
        );
        Bounds::new(desired.clamp(&self.display.origin, &max), size(edge, edge))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(cursor: (i32, i32), rect: (i32, i32, i32, i32)) -> RingPlacement {
        RingPlacement {
            display_id: DisplayId::from(42),
            cursor: point(DevicePixels(cursor.0), DevicePixels(cursor.1)),
            display: Bounds::new(
                point(DevicePixels(rect.0), DevicePixels(rect.1)),
                size(DevicePixels(rect.2 - rect.0), DevicePixels(rect.3 - rect.1)),
            ),
        }
    }

    /// The ring's edge in device pixels at `dpi`: [`WINDOW_SIZE`] DIP scaled,
    /// as [`RingPlacement::bounds`] scales it. Expectations are written in
    /// terms of it so they follow the ring's size rather than pin one.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test-sized DPI scaling of the ring's DIP edge"
    )]
    fn edge(dpi: u32) -> i32 {
        (f64::from(WINDOW_SIZE) * f64::from(dpi) / 96.0).round() as i32
    }

    fn half(dpi: u32) -> i32 {
        edge(dpi) / 2
    }

    fn device_point(x: i32, y: i32) -> Point<DevicePixels> {
        point(DevicePixels(x), DevicePixels(y))
    }

    #[test]
    fn native_monitor_identity_survives_logical_overlap() {
        // 100% primary [0,1920), 200% right-hand monitor [1920,4480).
        // The old DIP point (1000,400) also matched the primary display.
        let placement = placement((2000, 800), (1920, 0, 4480, 1440));
        let options = placement.window_options();
        assert_eq!(options.display_id, Some(DisplayId::from(42)));
        assert!(
            !options.show,
            "no frame may be shown at the default HWND DPI"
        );
        let bounds = placement.bounds(192);
        // Centred on the cursor, then held inside the monitor's left edge.
        assert_eq!(bounds.origin, device_point(1920, 800 - half(192)));
        assert_eq!(
            bounds.size,
            size(DevicePixels(edge(192)), DevicePixels(edge(192)))
        );
    }

    #[test]
    fn physical_center_is_independent_of_monitor_scale_and_global_origin() {
        for (cursor, rect, dpi) in [
            ((3000, 850), (1920, 0, 4480, 1440), 192),
            ((900, 600), (0, 0, 1920, 1080), 96),
            // 150% left, 125% above, and 175% below-left with nonzero X/Y.
            ((-1400, 450), (-2560, -200, 0, 1240), 144),
            ((-300, -700), (-640, -1440, 1920, 0), 120),
            ((-1200, 1600), (-1920, 1080, 0, 2520), 168),
        ] {
            let bounds = placement(cursor, rect).bounds(dpi);
            assert_eq!(
                bounds.origin,
                device_point(cursor.0 - half(dpi), cursor.1 - half(dpi)),
                "cursor {cursor:?} at {dpi} dpi"
            );
            assert_eq!(
                bounds.size,
                size(DevicePixels(edge(dpi)), DevicePixels(edge(dpi)))
            );
        }
    }

    /// A cursor, the monitor it is on, that monitor's DPI, and the origin the
    /// ring must land on there as a function of the DPI.
    type ClampCase = ((i32, i32), (i32, i32, i32, i32), u32, fn(u32) -> (i32, i32));

    #[test]
    fn monitor_seams_and_outer_edges_clamp_on_the_selected_side() {
        let cases: [ClampCase; 8] = [
            ((1919, 600), (0, 0, 1920, 1080), 96, |dpi| {
                (1920 - edge(dpi), 600 - half(dpi))
            }),
            ((1920, 600), (1920, 0, 4480, 1440), 192, |dpi| {
                (1920, 600 - half(dpi))
            }),
            ((4479, 1439), (1920, 0, 4480, 1440), 192, |dpi| {
                (4480 - edge(dpi), 1440 - edge(dpi))
            }),
            ((-1, 600), (-2560, -200, 0, 1240), 144, |dpi| {
                (-edge(dpi), 600 - half(dpi))
            }),
            ((0, 600), (0, 0, 1920, 1080), 96, |dpi| (0, 600 - half(dpi))),
            ((600, -1), (-640, -1440, 1920, 0), 120, |dpi| {
                (600 - half(dpi), -edge(dpi))
            }),
            ((600, 0), (0, 0, 1920, 1080), 96, |dpi| (600 - half(dpi), 0)),
            ((-640, -1440), (-640, -1440, 1920, 0), 120, |_| {
                (-640, -1440)
            }),
        ];
        for (cursor, rect, dpi, origin) in cases {
            let bounds = placement(cursor, rect).bounds(dpi);
            let (x, y) = origin(dpi);
            assert_eq!(
                bounds.origin,
                device_point(x, y),
                "cursor {cursor:?} at {dpi} dpi"
            );
        }
    }

    #[test]
    fn undersized_display_keeps_a_valid_clamp_range() {
        let bounds = placement((-100, -50), (-200, -100, 0, 0)).bounds(192);
        assert_eq!(bounds.origin, device_point(-200, -100));
        assert_eq!(
            bounds.size,
            size(DevicePixels(edge(192)), DevicePixels(edge(192)))
        );
    }
}

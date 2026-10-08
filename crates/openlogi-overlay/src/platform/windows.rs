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
        reason = "360 DIP scaled by a native display DPI fits in screen-sized i32 pixels"
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
        assert_eq!(bounds.origin, point(DevicePixels(1920), DevicePixels(440)));
        assert_eq!(bounds.size, size(DevicePixels(720), DevicePixels(720)));
    }

    #[test]
    fn physical_center_is_independent_of_monitor_scale_and_global_origin() {
        for (cursor, rect, dpi, origin, edge) in [
            ((3000, 850), (1920, 0, 4480, 1440), 192, (2640, 490), 720),
            ((900, 600), (0, 0, 1920, 1080), 96, (720, 420), 360),
            // 150% left, 125% above, and 175% below-left with nonzero X/Y.
            ((-1400, 450), (-2560, -200, 0, 1240), 144, (-1670, 180), 540),
            ((-300, -700), (-640, -1440, 1920, 0), 120, (-525, -925), 450),
            (
                (-1200, 1600),
                (-1920, 1080, 0, 2520),
                168,
                (-1515, 1285),
                630,
            ),
        ] {
            let bounds = placement(cursor, rect).bounds(dpi);
            assert_eq!(
                bounds.origin,
                point(DevicePixels(origin.0), DevicePixels(origin.1))
            );
            assert_eq!(bounds.size, size(DevicePixels(edge), DevicePixels(edge)));
        }
    }

    #[test]
    fn monitor_seams_and_outer_edges_clamp_on_the_selected_side() {
        for (cursor, rect, dpi, origin) in [
            ((1919, 600), (0, 0, 1920, 1080), 96, (1560, 420)),
            ((1920, 600), (1920, 0, 4480, 1440), 192, (1920, 240)),
            ((4479, 1439), (1920, 0, 4480, 1440), 192, (3760, 720)),
            ((-1, 600), (-2560, -200, 0, 1240), 144, (-540, 330)),
            ((0, 600), (0, 0, 1920, 1080), 96, (0, 420)),
            ((600, -1), (-640, -1440, 1920, 0), 120, (375, -450)),
            ((600, 0), (0, 0, 1920, 1080), 96, (420, 0)),
            ((-640, -1440), (-640, -1440, 1920, 0), 120, (-640, -1440)),
        ] {
            let bounds = placement(cursor, rect).bounds(dpi);
            assert_eq!(
                bounds.origin,
                point(DevicePixels(origin.0), DevicePixels(origin.1))
            );
        }
    }

    #[test]
    fn undersized_display_keeps_a_valid_clamp_range() {
        let bounds = placement((-100, -50), (-200, -100, 0, 0)).bounds(192);
        assert_eq!(bounds.origin, point(DevicePixels(-200), DevicePixels(-100)));
        assert_eq!(bounds.size, size(DevicePixels(720), DevicePixels(720)));
    }
}

//! Windows placement stays in physical virtual-desktop coordinates. GPUI's
//! per-monitor logical rectangles can overlap at mixed DPI, so neither cursor
//! monitor selection nor HWND positioning can use logical containment.

use gpui::{
    Bounds, DevicePixels, DisplayId, Pixels, Point, Size, WindowBounds, WindowOptions, point, size,
};

use crate::notice::{BOTTOM_MARGIN, notice_window_options};
use crate::ring::{Placement, WINDOW_SIZE, ring_window_options};

#[cfg(target_os = "windows")]
mod native;

/// One native cursor/monitor snapshot, kept through bootstrap-window creation.
pub(crate) struct RingPlacement {
    display_id: DisplayId,
    cursor: Point<DevicePixels>,
    display: Bounds<DevicePixels>,
    /// The selected monitor's effective DPI, used only to express the
    /// bootstrap bounds in that monitor's logical pixels.
    dpi: u32,
}

impl RingPlacement {
    /// The one set of windows to try: the window at the cursor, paired with
    /// how its view places the panel.
    pub(crate) fn openings(&self) -> Vec<Vec<(WindowOptions, Placement)>> {
        vec![vec![(self.window_options(), Placement::Window)]]
    }

    pub(crate) fn window_options(&self) -> WindowOptions {
        WindowOptions {
            display_id: Some(self.display_id),
            // GPUI's Windows backend only sets up DWM transparency for a window
            // created visible, so the ring opens shown — as a 1px bootstrap at
            // the selected monitor's centre until `native.rs` places it.
            window_bounds: Some(WindowBounds::Windowed(self.bootstrap_bounds())),
            show: true,
            ..ring_window_options()
        }
    }

    /// The 1px bootstrap at the monitor's centre, in its logical pixels.
    fn bootstrap_bounds(&self) -> Bounds<Pixels> {
        bootstrap_bounds(self.display, self.dpi)
    }

    /// GPUI's per-monitor scale factor, `dpi / 96`.
    #[cfg(test)]
    fn scale_factor(&self) -> f32 {
        scale_factor(self.dpi)
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

/// One native monitor snapshot for the notice window, kept through
/// bootstrap-window creation.
pub(crate) struct NoticePlacement {
    display_id: DisplayId,
    display: Bounds<DevicePixels>,
    /// The monitor without the taskbar (`rcWork`), where the notice sits.
    work: Bounds<DevicePixels>,
    /// The selected monitor's effective DPI, as for the ring.
    dpi: u32,
    /// The notice window's size in logical pixels.
    size: Size<Pixels>,
}

impl NoticePlacement {
    /// The notice window, opened as the same 1px bootstrap as the ring.
    pub(crate) fn openings(&self) -> Vec<WindowOptions> {
        vec![WindowOptions {
            display_id: Some(self.display_id),
            window_bounds: Some(WindowBounds::Windowed(bootstrap_bounds(
                self.display,
                self.dpi,
            ))),
            show: true,
            ..notice_window_options()
        }]
    }

    /// The notice at the bottom centre of the work area, above the taskbar.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "notice dimensions scaled by a native display DPI fit in screen-sized i32 pixels"
    )]
    fn bounds(&self, dpi: u32) -> Bounds<DevicePixels> {
        let scaled = |logical: f32| {
            DevicePixels((f64::from(logical) * f64::from(dpi) / 96.0).round() as i32)
        };
        let width = scaled(self.size.width.as_f32());
        let height = scaled(self.size.height.as_f32());
        let work = self.work;
        let desired = point(
            work.left() + DevicePixels((work.size.width.0 - width.0) / 2),
            work.bottom() - height - scaled(BOTTOM_MARGIN),
        );
        // A work area smaller than the notice anchors it to the top-left, as
        // an undersized display does for the ring.
        let max = point(
            (work.right() - width).max(work.left()),
            (work.bottom() - height).max(work.top()),
        );
        Bounds::new(desired.clamp(&work.origin, &max), size(width, height))
    }
}

/// The 1px bootstrap at the monitor's centre, in its logical pixels.
///
/// GPUI validates requested bounds by scaling their centre with the requested
/// monitor's own factor. Bounds that land elsewhere are replaced by its
/// default (large, and here visible) window, so they must be expressed for
/// this monitor rather than as a global logical point.
fn bootstrap_bounds(display: Bounds<DevicePixels>, dpi: u32) -> Bounds<Pixels> {
    Bounds::new(display.center(), size(DevicePixels(1), DevicePixels(1)))
        .to_pixels(scale_factor(dpi))
}

/// GPUI's per-monitor scale factor, `dpi / 96`.
#[expect(
    clippy::cast_precision_loss,
    reason = "effective DPIs are a few hundred, exactly representable in f32"
)]
fn scale_factor(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::stack_size;

    fn placement(cursor: (i32, i32), rect: (i32, i32, i32, i32)) -> RingPlacement {
        RingPlacement {
            display_id: DisplayId::from(42),
            cursor: point(DevicePixels(cursor.0), DevicePixels(cursor.1)),
            display: Bounds::new(
                point(DevicePixels(rect.0), DevicePixels(rect.1)),
                size(DevicePixels(rect.2 - rect.0), DevicePixels(rect.3 - rect.1)),
            ),
            dpi: 96,
        }
    }

    /// The single positioned window this placement opens.
    #[track_caller]
    fn opened(placement: &RingPlacement) -> WindowOptions {
        let mut windows = placement.openings().into_iter().flatten();
        let (options, panel) = windows.next().expect("a Windows ring opens a window");
        assert!(windows.next().is_none(), "a Windows ring opens one window");
        assert_eq!(panel, Placement::Window);
        options
    }

    /// Mirror GPUI's validation: the requested bounds, scaled by the requested
    /// monitor's own factor, must land back on that monitor as a 1px window.
    #[track_caller]
    fn assert_bootstrap_on_display(placement: &RingPlacement) {
        let Some(WindowBounds::Windowed(bounds)) = opened(placement).window_bounds else {
            panic!("the ring must request windowed bootstrap bounds");
        };
        let device = bounds.to_device_pixels(placement.scale_factor());
        assert!(
            placement.display.contains(&device.center()),
            "bootstrap {device:?} is off its monitor {:?}",
            placement.display
        );
        assert_eq!(device.size, size(DevicePixels(1), DevicePixels(1)));
    }

    #[test]
    fn native_monitor_identity_survives_logical_overlap() {
        // 100% primary [0,1920), 200% right-hand monitor [1920,4480). The
        // bootstrap's logical centre (1600,360) also lies inside the primary's
        // logical rectangle, yet must resolve to the 200% monitor.
        let placement = RingPlacement {
            dpi: 192,
            ..placement((2000, 800), (1920, 0, 4480, 1440))
        };
        let options = opened(&placement);
        assert_eq!(options.display_id, Some(DisplayId::from(42)));
        assert!(
            options.show,
            "GPUI only applies DWM transparency to a window created visible"
        );
        assert_bootstrap_on_display(&placement);
        let bounds = placement.bounds(192);
        assert_eq!(bounds.origin, point(DevicePixels(1920), DevicePixels(440)));
        assert_eq!(bounds.size, size(DevicePixels(720), DevicePixels(720)));
    }

    #[test]
    fn bootstrap_opens_on_each_mixed_dpi_monitor() {
        // A 150% primary at the origin and a 125% monitor to its left: a
        // global (0,0) bootstrap matched only the primary, so on the left
        // monitor GPUI fell back to its large default window instead.
        for (cursor, rect, dpi) in [
            ((700, 1200), (0, 0, 1440, 2560), 144),
            ((-1200, 800), (-2560, 105, 0, 1545), 120),
        ] {
            assert_bootstrap_on_display(&RingPlacement {
                dpi,
                ..placement(cursor, rect)
            });
        }
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

    fn rect_bounds(rect: (i32, i32, i32, i32)) -> Bounds<DevicePixels> {
        Bounds::new(
            point(DevicePixels(rect.0), DevicePixels(rect.1)),
            size(DevicePixels(rect.2 - rect.0), DevicePixels(rect.3 - rect.1)),
        )
    }

    fn notice(
        display: (i32, i32, i32, i32),
        work: (i32, i32, i32, i32),
        dpi: u32,
    ) -> NoticePlacement {
        NoticePlacement {
            display_id: DisplayId::from(7),
            display: rect_bounds(display),
            work: rect_bounds(work),
            dpi,
            // One card: 344×88 logical pixels.
            size: stack_size(1),
        }
    }

    #[test]
    fn a_notice_sits_centred_above_the_taskbar() {
        // 1920×1080 at 100%, with a 48px taskbar along the bottom.
        let bounds = notice((0, 0, 1920, 1080), (0, 0, 1920, 1032), 96).bounds(96);
        assert_eq!(bounds.size, size(DevicePixels(344), DevicePixels(88)));
        // Centred: (1920 - 344) / 2 = 788. Above the taskbar by the margin:
        // 1032 - 88 - 48 = 896.
        assert_eq!(bounds.origin, point(DevicePixels(788), DevicePixels(896)));
    }

    #[test]
    fn a_notice_scales_with_its_monitor_and_follows_its_work_area() {
        // A 200% monitor right of the primary, taskbar on its left edge.
        let bounds = notice((1920, 0, 4480, 1440), (2016, 0, 4480, 1440), 192).bounds(192);
        assert_eq!(bounds.size, size(DevicePixels(688), DevicePixels(176)));
        // (2464 - 688) / 2 = 888 into the work area; 1440 - 176 - 96 = 1168.
        assert_eq!(bounds.origin, point(DevicePixels(2904), DevicePixels(1168)));
    }

    #[test]
    fn a_notice_opens_as_a_bootstrap_on_its_monitor() {
        let placement = notice((-2560, 105, 0, 1545), (-2560, 105, 0, 1497), 120);
        let options = placement
            .openings()
            .into_iter()
            .next()
            .expect("a Windows notice opens one window");
        assert_eq!(options.display_id, Some(DisplayId::from(7)));
        assert!(
            options.show,
            "GPUI only applies DWM transparency to a window created visible"
        );
        let Some(WindowBounds::Windowed(bounds)) = options.window_bounds else {
            panic!("the notice must request windowed bootstrap bounds");
        };
        let device = bounds.to_device_pixels(scale_factor(120));
        assert!(placement.display.contains(&device.center()));
        assert_eq!(device.size, size(DevicePixels(1), DevicePixels(1)));
    }

    #[test]
    fn a_work_area_smaller_than_the_notice_anchors_it_top_left() {
        let bounds = notice((0, 0, 200, 60), (0, 0, 200, 60), 96).bounds(96);
        assert_eq!(bounds.origin, point(DevicePixels(0), DevicePixels(0)));
    }
}

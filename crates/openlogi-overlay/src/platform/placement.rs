//! GPUI placement on macOS and Linux.

use gpui::{Bounds, DisplayId, Pixels, Point, Size, WindowBounds, WindowOptions, point, px};

use crate::ring::{WINDOW_SIZE, clamp_window_origin, ring_window_options};

/// Captured placement for one invocation, before its window is opened.
pub(crate) struct RingPlacement {
    display_id: Option<DisplayId>,
    bounds: Bounds<Pixels>,
}

impl RingPlacement {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Windows placement capture is fallible"
    )]
    pub(crate) fn capture(cx: &mut gpui::App) -> anyhow::Result<Self> {
        let cursor = openlogi_hook::cursor_position();
        let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
        // macOS GPUI bounds are display-relative; translate the global cursor
        // into the native display's space before clamping.
        let native_display = cursor
            .as_ref()
            .and_then(|cursor| super::display_containing(cursor.x, cursor.y));
        let (display_id, center, display_bounds) =
            if let (Some(cursor), Some(display)) = (&cursor, native_display) {
                (
                    Some(gpui::DisplayId::from(display.id)),
                    point(
                        px((cursor.x - display.origin.0) as f32),
                        px((cursor.y - display.origin.1) as f32),
                    ),
                    Some(Bounds::new(
                        Point::default(),
                        Size::new(px(display.size.0 as f32), px(display.size.1 as f32)),
                    )),
                )
            } else {
                // Linux, or a failed macOS lookup: preserve the GPUI fallback.
                let cursor_point = cursor
                    .as_ref()
                    .map(|cursor| point(px(cursor.x as f32), px(cursor.y as f32)));
                let display = cursor_point
                    .and_then(|cursor| {
                        cx.displays()
                            .into_iter()
                            .find(|display| display.bounds().contains(&cursor))
                    })
                    .or_else(|| cx.primary_display());
                let center = cursor_point
                    .or_else(|| display.as_ref().map(|display| display.bounds().center()))
                    .unwrap_or_default();
                let bounds = display.as_ref().map(|display| display.bounds());
                (display.map(|display| display.id()), center, bounds)
            };
        let desired_origin = point(center.x - size.width / 2.0, center.y - size.height / 2.0);
        let origin = display_bounds.map_or(desired_origin, |display_bounds| {
            clamp_window_origin(desired_origin, size, display_bounds)
        });
        Ok(Self {
            bounds: Bounds::new(origin, size),
            display_id,
        })
    }

    pub(crate) fn window_options(&self) -> WindowOptions {
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(self.bounds)),
            display_id: self.display_id,
            ..ring_window_options()
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the Windows implementation consumes the captured geometry and can fail"
    )]
    pub(crate) fn show(self, _window: &mut gpui::Window) -> anyhow::Result<()> {
        super::configure_windows();
        Ok(())
    }
}

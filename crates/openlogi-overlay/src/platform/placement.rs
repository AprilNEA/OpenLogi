//! GPUI placement on macOS and Linux.

use gpui::{
    Bounds, DisplayId, Pixels, PlatformDisplay, Point, Size, WindowBounds, WindowOptions, point, px,
};
use tracing::warn;

use crate::notice::{bottom_centre, notice_window_options};
use crate::ring::{Placement, WINDOW_SIZE, clamp_window_origin, ring_window_options};

/// Captured placement for one invocation, before its windows are opened.
pub(crate) struct RingPlacement {
    alternatives: Vec<Vec<(WindowOptions, Placement)>>,
}

impl RingPlacement {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Windows placement capture is fallible"
    )]
    pub(crate) fn capture(cx: &mut gpui::App) -> anyhow::Result<Self> {
        if on_wayland() {
            let displays = cx.displays();
            if !displays.is_empty() {
                // A layer surface names its display. An ordinary window goes
                // where the compositor puts it, and GNOME, which has no layer
                // shell, stacks every display's window on one (#1901).
                let layer_shell = displays
                    .iter()
                    .filter_map(|display| layer_shell_ring(display.as_ref()))
                    .map(|options| (options, Placement::AtPointer))
                    .collect();
                let covering = displays
                    .iter()
                    .map(|display| {
                        (
                            covering_window_options(display.as_ref()),
                            Placement::AtPointer,
                        )
                    })
                    .collect();
                return Ok(Self {
                    alternatives: [layer_shell, covering]
                        .into_iter()
                        .filter(|windows: &Vec<_>| !windows.is_empty())
                        .collect(),
                });
            }
            warn!("no displays reported; falling back to a positioned Actions Ring window");
        }
        Ok(Self {
            alternatives: vec![vec![positioned_window(cx)]],
        })
    }

    /// Sets of windows to try in order, each window's options paired with how
    /// its view places the panel. The first set that opens a window is the ring.
    pub(crate) fn openings(&mut self) -> Vec<Vec<(WindowOptions, Placement)>> {
        std::mem::take(&mut self.alternatives)
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

/// Captured placement for the notice window, before it opens.
pub(crate) struct NoticePlacement {
    attempts: Vec<WindowOptions>,
}

impl NoticePlacement {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Windows placement capture is fallible"
    )]
    pub(crate) fn capture(cx: &mut gpui::App, size: Size<Pixels>) -> anyhow::Result<Self> {
        let attempts = if on_wayland() {
            wayland_notice_windows(size)
        } else {
            vec![positioned_notice(cx, size)]
        };
        Ok(Self { attempts })
    }

    /// Window options to try in order; the first that opens is the notice.
    pub(crate) fn openings(&mut self) -> Vec<WindowOptions> {
        std::mem::take(&mut self.attempts)
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

/// The display under the cursor, in the space GPUI positions its windows in.
struct CursorScreen {
    display_id: Option<DisplayId>,
    /// The cursor, when it could be read.
    cursor: Option<Point<Pixels>>,
    /// The whole display.
    bounds: Option<Bounds<Pixels>>,
    /// The part of the display free of the menu bar and the Dock, where the
    /// platform reports it (macOS).
    usable: Option<Bounds<Pixels>>,
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
fn cursor_screen(cx: &gpui::App) -> CursorScreen {
    let cursor = openlogi_hook::cursor_position();
    // macOS GPUI bounds are display-relative; translate the global cursor
    // into the native display's space.
    let native_display = cursor
        .as_ref()
        .and_then(|cursor| super::display_containing(cursor.x, cursor.y));
    if let (Some(cursor), Some(display)) = (&cursor, native_display) {
        return CursorScreen {
            display_id: Some(DisplayId::from(display.id)),
            cursor: Some(point(
                px((cursor.x - display.origin.0) as f32),
                px((cursor.y - display.origin.1) as f32),
            )),
            bounds: Some(Bounds::new(
                Point::default(),
                Size::new(px(display.size.0 as f32), px(display.size.1 as f32)),
            )),
            usable: super::usable_area(&display),
        };
    }
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
    CursorScreen {
        display_id: display.as_ref().map(|display| display.id()),
        cursor: cursor_point,
        bounds: display.map(|display| display.bounds()),
        usable: None,
    }
}

fn positioned_window(cx: &gpui::App) -> (WindowOptions, Placement) {
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    let screen = cursor_screen(cx);
    let center = screen
        .cursor
        .or_else(|| screen.bounds.map(|bounds| bounds.center()))
        .unwrap_or_default();
    let desired_origin = point(center.x - size.width / 2.0, center.y - size.height / 2.0);
    let origin = screen.bounds.map_or(desired_origin, |display_bounds| {
        clamp_window_origin(desired_origin, size, display_bounds)
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size))),
        display_id: screen.display_id,
        ..ring_window_options()
    };
    (options, Placement::Window)
}

/// The notice at the bottom centre of the cursor's display, above the Dock
/// where the platform reports it.
fn positioned_notice(cx: &gpui::App, size: Size<Pixels>) -> WindowOptions {
    let screen = cursor_screen(cx);
    let origin = screen
        .usable
        .or(screen.bounds)
        .map_or_else(Point::default, |area| bottom_centre(area, size));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size))),
        display_id: screen.display_id,
        ..notice_window_options()
    }
}

/// Wayland lets no client place its own window. A layer-shell surface is the
/// sanctioned way to put one at a screen edge, so try that first; a
/// compositor without `wlr-layer-shell` gets an ordinary window it places
/// itself.
fn wayland_notice_windows(size: Size<Pixels>) -> Vec<WindowOptions> {
    let bounds = Some(WindowBounds::Windowed(Bounds::new(Point::default(), size)));
    let plain = WindowOptions {
        window_bounds: bounds,
        ..notice_window_options()
    };
    layer_shell_notice(bounds)
        .into_iter()
        .chain([plain])
        .collect()
}

/// The notice as a layer-shell surface on the bottom edge, above other
/// windows and never taking the keyboard, unless the compositor refused one.
#[cfg(target_os = "linux")]
fn layer_shell_notice(bounds: Option<WindowBounds>) -> Option<WindowOptions> {
    use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};

    if super::layer_shell_refused() {
        return None;
    }
    Some(WindowOptions {
        kind: gpui::WindowKind::LayerShell(LayerShellOptions {
            namespace: crate::notice::NOTICE_APP_ID.to_string(),
            layer: Layer::Overlay,
            anchor: Anchor::BOTTOM,
            margin: Some((px(0.0), px(0.0), px(crate::notice::BOTTOM_MARGIN), px(0.0))),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        }),
        window_bounds: bounds,
        ..notice_window_options()
    })
}

#[cfg(not(target_os = "linux"))]
const fn layer_shell_notice(_bounds: Option<WindowBounds>) -> Option<WindowOptions> {
    None
}

/// The ring's window on `display` as a layer-shell surface over the whole
/// display, above every window and never taking the keyboard, unless the
/// compositor refused one.
///
/// Anchored to all four edges with no size of its own, the surface takes
/// whatever size the compositor gives that display. Display bounds would not
/// do: GPUI derives them from an integer scale, which is wrong under
/// fractional scaling.
#[cfg(target_os = "linux")]
fn layer_shell_ring(display: &dyn PlatformDisplay) -> Option<WindowOptions> {
    use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};

    if super::layer_shell_refused() {
        return None;
    }
    Some(WindowOptions {
        kind: gpui::WindowKind::LayerShell(LayerShellOptions {
            namespace: crate::ring::RING_APP_ID.to_string(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            // Cover the panels too, rather than the area they leave free.
            exclusive_zone: Some(px(-1.0)),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::default())),
        display_id: Some(display.id()),
        ..ring_window_options()
    })
}

#[cfg(not(target_os = "linux"))]
const fn layer_shell_ring(_display: &dyn PlatformDisplay) -> Option<WindowOptions> {
    None
}

/// Transparent maximized window covering `display`; fullscreen would composite it against black.
fn covering_window_options(display: &dyn PlatformDisplay) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Maximized(display.bounds())),
        display_id: Some(display.id()),
        window_decorations: Some(gpui::WindowDecorations::Client),
        ..ring_window_options()
    }
}

/// Report whether GPUI's own backend selection is Wayland.
fn on_wayland() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        gpui::guess_compositor() == "Wayland"
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        false
    }
}

/// A rectangle in AppKit's global space: the origin is the primary display's
/// bottom-left corner, and `y` grows upwards.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CocoaRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// The usable part of a screen in its display's own top-left-origin space,
/// when the screen with `frame` is the display at `display_origin`.
///
/// `display_origin` is in CoreGraphics' global space, whose origin is the
/// primary display's top-left corner and whose `y` grows downwards. A
/// screen's frame lands there once flipped against the primary display's
/// height; two displays never share an origin, so a match is the display.
#[cfg(any(target_os = "macos", test))]
#[expect(
    clippy::cast_possible_truncation,
    reason = "screen geometry is a few thousand points, exactly usable as GPUI f32 pixels"
)]
pub(crate) fn usable_in_display(
    frame: CocoaRect,
    visible: CocoaRect,
    primary_height: f64,
    display_origin: (f64, f64),
) -> Option<Bounds<Pixels>> {
    let frame_top = frame.y + frame.height;
    let top_left = (frame.x, primary_height - frame_top);
    let same_display =
        (top_left.0 - display_origin.0).abs() < 0.5 && (top_left.1 - display_origin.1).abs() < 0.5;
    same_display.then(|| {
        Bounds::new(
            point(
                px((visible.x - frame.x) as f32),
                px((frame_top - (visible.y + visible.height)) as f32),
            ),
            Size::new(px(visible.width as f32), px(visible.height as f32)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x: f64, y: f64, width: f64, height: f64) -> CocoaRect {
        CocoaRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_primary_display_loses_its_menu_bar_and_dock() {
        // 1512×982 primary: a 37pt menu bar at the top, a 70pt Dock at the
        // bottom. AppKit's y grows upwards, so the Dock is the low edge.
        let usable = usable_in_display(
            rect(0.0, 0.0, 1512.0, 982.0),
            rect(0.0, 70.0, 1512.0, 875.0),
            982.0,
            (0.0, 0.0),
        );
        assert_eq!(
            usable,
            Some(Bounds::new(
                point(px(0.0), px(37.0)),
                Size::new(px(1512.0), px(875.0)),
            ))
        );
    }

    #[test]
    fn a_secondary_display_is_found_by_its_flipped_origin() {
        // A 1920×1080 display above-right of a 1512×982 primary. CoreGraphics
        // puts its top-left at (1512, -1080); AppKit puts its bottom-left at
        // (1512, 982). Its menu bar is 25pt and it has no Dock.
        let frame = rect(1512.0, 982.0, 1920.0, 1080.0);
        let visible = rect(1512.0, 982.0, 1920.0, 1055.0);
        assert_eq!(
            usable_in_display(frame, visible, 982.0, (1512.0, -1080.0)),
            Some(Bounds::new(
                point(px(0.0), px(25.0)),
                Size::new(px(1920.0), px(1055.0)),
            ))
        );
        assert_eq!(
            usable_in_display(frame, visible, 982.0, (0.0, 0.0)),
            None,
            "a screen is only the display whose origin it shares"
        );
    }

    #[test]
    fn a_dock_on_the_left_moves_the_usable_area_right() {
        let usable = usable_in_display(
            rect(0.0, 0.0, 1440.0, 900.0),
            rect(64.0, 0.0, 1376.0, 875.0),
            900.0,
            (0.0, 0.0),
        );
        assert_eq!(
            usable,
            Some(Bounds::new(
                point(px(64.0), px(25.0)),
                Size::new(px(1376.0), px(875.0)),
            ))
        );
    }
}

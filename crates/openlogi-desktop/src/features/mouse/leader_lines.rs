//! Canvas-painted leader lines from each hotspot to its side-label anchor.
//!
//! Each polyline is hotspot-centre → short horizontal stub → diagonal to the
//! label anchor. The control being interacted with gets a blue, thicker line;
//! the other controls keep muted lines so their mapping remains visible.

use gpui::{Bounds, PathBuilder, Pixels, Point, Window, hsla, point, px, rgb};

use super::hotspots::{Hotspot, MouseControlId};
use crate::ui::theme::ACCENT_BLUE;

/// Length of the horizontal stub before turning toward the label.
/// Kept small enough to fit inside the gap between mouse and card so
/// the diagonal doesn't start inside the card.
const STUB: f32 = 10.;

/// Which side of the mouse a label sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct Label {
    pub(crate) id: MouseControlId,
    pub side: Side,
    /// Y of the label anchor, in mouse-canvas coords (i.e. relative to the
    /// canvas's top-left, not the mouse silhouette's top-left).
    pub y: f32,
}

/// Geometry the view supplies once per render. `mouse_origin` is the
/// silhouette's top-left in *canvas-local* coords; `card_edge_inset` is
/// how far the card's inner edge sits from the mouse silhouette's
/// matching side (symmetric across left / right since the gutter is the
/// same width on either side). Bundled into a struct so [`paint`] and
/// [`paint_one`] don't trip clippy's argument-count limit.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub mouse_origin: Point<Pixels>,
    pub mouse_w: f32,
    pub card_edge_inset: f32,
}

/// Paint every leader line. Hotspot coords are mouse-local; label `y`
/// is canvas-local. Everything is converted to window-absolute before
/// being handed to `PathBuilder` — `paint_path` expects absolute coords
/// and there is no implicit canvas-to-window transform.
pub fn paint(
    canvas_bounds: Bounds<Pixels>,
    geometry: Geometry,
    hotspots: &[Hotspot],
    labels: &[Label],
    highlighted: Option<MouseControlId>,
    window: &mut Window,
) {
    for label in labels {
        let Some(hotspot) = hotspots.iter().find(|h| h.id == label.id) else {
            continue;
        };
        paint_one(
            canvas_bounds.origin,
            geometry,
            *hotspot,
            *label,
            highlighted == Some(label.id),
            window,
        );
    }
}

fn paint_one(
    canvas_screen_origin: Point<Pixels>,
    geometry: Geometry,
    hotspot: Hotspot,
    label: Label,
    highlight: bool,
    window: &mut Window,
) {
    let Geometry {
        mouse_origin: mouse_origin_local,
        mouse_w,
        card_edge_inset,
    } = geometry;
    // Mouse silhouette's top-left in window-absolute coords. Every other
    // coordinate is derived from this so we don't accidentally mix
    // coordinate systems again.
    let mouse_screen = canvas_screen_origin + mouse_origin_local;

    let (hx, hy) = hotspot.center();
    let hotspot_centre = mouse_screen + point(px(hx), px(hy));

    // Stub ends inside the gutter; anchor lands flush with the card's
    // mouse-facing edge so the diagonal touches the card without
    // overshooting through the text.
    let (stub_x, anchor_x) = match label.side {
        Side::Left => (
            mouse_screen.x - px(STUB),
            mouse_screen.x - px(card_edge_inset),
        ),
        Side::Right => (
            mouse_screen.x + px(mouse_w) + px(STUB),
            mouse_screen.x + px(mouse_w) + px(card_edge_inset),
        ),
    };
    let stub = Point {
        x: stub_x,
        y: hotspot_centre.y,
    };
    let anchor = Point {
        x: anchor_x,
        y: mouse_screen.y + px(label.y),
    };

    let width = if highlight { px(2.) } else { px(1.) };
    let mut path = PathBuilder::stroke(width);
    path.move_to(hotspot_centre);
    path.line_to(stub);
    path.line_to(anchor);

    if let Ok(built) = path.build() {
        if highlight {
            window.paint_path(built, rgb(ACCENT_BLUE));
        } else {
            // Muted gray — readable against the dark background without
            // competing with the highlighted line.
            window.paint_path(built, hsla(0., 0., 0.55, 0.35));
        }
    }
}

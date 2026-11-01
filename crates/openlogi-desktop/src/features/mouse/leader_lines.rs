//! Canvas-painted leader lines from each hotspot to its side-label anchor.
//!
//! Each polyline is hotspot-centre → 45° run to the card's height → straight
//! into the card, like a routed trace. A card level with its control gets a
//! single straight line. The control being interacted with gets a blue,
//! thicker line; the other controls keep muted lines so their mapping remains
//! visible.

use gpui::{Bounds, PathBuilder, Pixels, Point, Window, hsla, point, px, rgb};

use super::hotspots::{Hotspot, MouseControlId};
use crate::ui::theme::ACCENT_BLUE;

/// How far past the silhouette a leader's 45° run may reach. Kept small
/// enough to fit inside the gap between mouse and card so the run never ends
/// inside the card; a run that would reach further steepens instead.
pub(crate) const STUB: f32 = 10.;

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

    let points = route(
        hotspot.center(),
        label.side,
        label.y,
        mouse_w,
        card_edge_inset,
    );
    let width = if highlight { px(2.) } else { px(1.) };
    let mut path = PathBuilder::stroke(width);
    for (i, &(x, y)) in points.iter().enumerate() {
        let corner = mouse_screen + point(px(x), px(y));
        if i == 0 {
            path.move_to(corner);
        } else {
            path.line_to(corner);
        }
    }

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

/// A leader's corners in mouse-local coordinates: from the hotspot centre a
/// 45° run to the card's height `label_y`, then straight to the card's
/// mouse-facing edge, `card_edge_inset` outside the silhouette. The run ends
/// at most [`STUB`] past the silhouette; further out it steepens instead.
pub fn route(
    (hx, hy): (f32, f32),
    side: Side,
    label_y: f32,
    mouse_w: f32,
    card_edge_inset: f32,
) -> Vec<(f32, f32)> {
    let rise = (label_y - hy).abs();
    if rise < 0.5 {
        let anchor_x = match side {
            Side::Left => -card_edge_inset,
            Side::Right => mouse_w + card_edge_inset,
        };
        return vec![(hx, hy), (anchor_x, label_y)];
    }
    let (turn_x, anchor_x) = match side {
        Side::Left => ((hx - rise).max((-STUB).min(hx)), -card_edge_inset),
        Side::Right => (
            (hx + rise).min((mouse_w + STUB).max(hx)),
            mouse_w + card_edge_inset,
        ),
    };
    vec![(hx, hy), (turn_x, label_y), (anchor_x, label_y)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_card_gets_one_straight_leader() {
        assert_eq!(
            route((120., 80.), Side::Left, 80., 300., 24.),
            [(120., 80.), (-24., 80.)]
        );
    }

    #[test]
    fn a_raised_card_gets_a_45_degree_run_then_a_straight_leader() {
        assert_eq!(
            route((120., 80.), Side::Right, 40., 300., 24.),
            [(120., 80.), (160., 40.), (324., 40.)]
        );
    }

    #[test]
    fn a_run_that_would_leave_the_gutter_steepens() {
        assert_eq!(
            route((20., 200.), Side::Left, 100., 300., 24.),
            [(20., 200.), (-STUB, 100.), (-24., 100.)]
        );
    }
}

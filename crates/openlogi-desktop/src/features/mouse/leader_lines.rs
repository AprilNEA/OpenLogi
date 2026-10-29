//! Canvas-painted leader lines from each hotspot to its side-label anchor,
//! and the layout rules the mouse and keyboard callouts share.
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

/// Whether two polylines cross. Segments that only touch at an end do not
/// count, so two leaders meeting the same card edge are fine.
pub fn routes_cross(a: &[(f32, f32)], b: &[(f32, f32)]) -> bool {
    a.windows(2)
        .any(|s| b.windows(2).any(|t| segments_cross(s[0], s[1], t[0], t[1])))
}

fn segments_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
    fn side(p: (f32, f32), q: (f32, f32), r: (f32, f32)) -> i8 {
        let cross = (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        if cross > 1e-3 {
            1
        } else if cross < -1e-3 {
            -1
        } else {
            0
        }
    }
    side(a, b, c) * side(a, b, d) < 0 && side(c, d, a) * side(c, d, b) < 0
}

/// Positions for a row of cards that keeps the given order: each card as
/// close to its `ideal` position as possible (least squares), neighbours at
/// least `spacing` apart, every position within `lo..=hi`. `ideal` must
/// already read in the order the cards should, so a card never jumps past
/// its neighbour and the leaders keep their order. Cards that cannot all fit
/// overflow both ends evenly.
#[expect(
    clippy::cast_precision_loss,
    reason = "a row holds at most a few dozen callouts — well under f32 mantissa"
)]
pub fn spread_in_order(ideal: &[f32], spacing: f32, lo: f32, hi: f32) -> Vec<f32> {
    if ideal.is_empty() {
        return Vec::new();
    }
    // Shifting each target back by `index * spacing` turns "at least
    // `spacing` apart" into "non-decreasing", which pooling adjacent
    // violators solves in one pass: a run that would cross is replaced by
    // its mean.
    let mut runs: Vec<(f32, usize)> = Vec::with_capacity(ideal.len());
    for (index, target) in ideal.iter().enumerate() {
        runs.push((target - spacing * index as f32, 1));
        while let [.., (sum_a, len_a), (sum_b, len_b)] = runs[..] {
            if sum_a / len_a as f32 <= sum_b / len_b as f32 {
                break;
            }
            runs.truncate(runs.len() - 2);
            runs.push((sum_a + sum_b, len_a + len_b));
        }
    }
    let last_start = hi - spacing * (ideal.len() - 1) as f32;
    let (first, last) = if last_start >= lo {
        (lo, last_start)
    } else {
        let centred = f32::midpoint(lo, last_start);
        (centred, centred)
    };
    runs.into_iter()
        .flat_map(|(sum, len)| std::iter::repeat_n((sum / len as f32).clamp(first, last), len))
        .enumerate()
        .map(|(index, start)| start + spacing * index as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_keeps_ideal_positions_that_already_fit() {
        assert_eq!(
            spread_in_order(&[10., 50., 120.], 30., 0., 200.),
            [10., 50., 120.]
        );
    }

    #[test]
    fn spread_pushes_a_crowded_run_apart_around_its_mean() {
        let ys = spread_in_order(&[100., 105., 110.], 20., 0., 300.);
        assert_eq!(ys, [85., 105., 125.]);
    }

    #[test]
    fn spread_never_reorders_and_stays_in_bounds() {
        let ys = spread_in_order(&[5., 0., 300.], 40., 20., 280.);
        assert_eq!(ys, [20., 60., 280.]);
    }

    #[test]
    fn spread_centres_a_row_too_long_for_its_bounds() {
        let ys = spread_in_order(&[0., 0., 0.], 100., 0., 100.);
        assert_eq!(ys, [-50., 50., 150.]);
    }

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

    #[test]
    fn crossing_and_touching_polylines_are_told_apart() {
        let down = [(0., 0.), (10., 10.)];
        let up = [(0., 10.), (10., 0.)];
        let beside = [(10., 10.), (20., 10.)];
        assert!(routes_cross(&down, &up));
        assert!(!routes_cross(&down, &beside));
    }
}

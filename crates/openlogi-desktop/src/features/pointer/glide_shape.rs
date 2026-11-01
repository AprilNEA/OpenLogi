//! The glide-shape picker: one clickable tile per [`GlideShape`], each
//! drawing its glide's speed over time with the curve the agent glides by
//! ([`GlideCurve`]). The curves follow the other glide settings — a faster
//! max speed or a longer notch raises their top, a longer glide stretches
//! them, a repeat speed-up shows as a faint, higher copy — with every effect
//! exaggerated so a change is plain to see.

use std::time::Duration;

use gpui::{
    Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, SharedString, Styled, Window,
    canvas, div, point, px, rgb,
};
use gpui_component::{Selectable as _, h_flex, v_flex};
use openlogi_core::config::{GlideShape, MomentumParams, MomentumScrolling as M};
use openlogi_core::scroll::GlideCurve;

use super::momentum::{MomentumScope, commit};
use crate::ui::choice_card::ChoiceCard;
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{ACCENT_BLUE, Palette, Typography as _};

/// Points each curve is drawn through.
const SAMPLES: u16 = 48;
/// Height of a tile's plot.
const PLOT_HEIGHT: f32 = 64.0;

/// One tile's curve as fractions of its plot: how high it starts, how far
/// right it runs before it stops, and how high a repeated spin starts.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CurveGeometry {
    shape: GlideShape,
    top: f32,
    width: f32,
    repeat_top: Option<f32>,
}

/// Where `value`, already inside `min..=max`, sits in that range, `0.0` to
/// `1.0`, on a log scale: each setting's stops grow by ratios, so equal
/// steps read as equal moves.
fn position(value: f64, min: u16, max: u16) -> f64 {
    let (min, max) = (f64::from(min), f64::from(max));
    (value / min).ln() / (max / min).ln()
}

/// `shape`'s curve under `params` — the settings the sliders show, a dragged
/// thumb's stop included, so the curves move with the drag.
#[expect(
    clippy::cast_possible_truncation,
    reason = "plot fractions in 0..=1 fit an f32"
)]
fn geometry(shape: GlideShape, params: &MomentumParams) -> CurveGeometry {
    let speed = position(
        params.max_glide_speed(),
        M::MAX_GLIDE_SPEED_MIN,
        M::MAX_GLIDE_SPEED_MAX,
    );
    let notch = position(
        params.pixels_per_notch(),
        M::PIXELS_PER_NOTCH_MIN,
        M::PIXELS_PER_NOTCH_MAX,
    );
    let longest = Duration::from_millis(u64::from(M::GLIDE_MS_MAX));
    let stretch = position(
        params.glide_length().as_secs_f64() * 1000.0,
        M::GLIDE_MS_MIN,
        M::GLIDE_MS_MAX,
    );
    // The top spans most of the plot across the speed range, and the notch
    // distance — which sets how fast a spin launches — nudges it too.
    let top = (0.3 + 0.7 * speed) * (0.7 + 0.3 * notch);
    // Every shape on one time axis, the floatiest at the longest length
    // spanning the plot; the length itself is exaggerated.
    let duration = |shape| {
        GlideCurve::new(1.0, shape, longest)
            .duration()
            .as_secs_f64()
    };
    let width = duration(shape) / duration(GlideShape::Floaty) * (0.3 + 0.7 * stretch);
    let boost = params.repeat_boost();
    let repeat_top = (boost > 1.0).then(|| (top * (1.0 + 2.0 * (boost - 1.0))).min(1.0) as f32);
    CurveGeometry {
        shape,
        top: top as f32,
        width: width as f32,
        repeat_top,
    }
}

/// The curve through `plot`, from its top-left start to where it reaches the
/// baseline, then along the baseline to the plot's right edge.
fn curve_path(plot: Bounds<Pixels>, shape: GlideShape, top: f32, width: f32) -> PathBuilder {
    let curve = GlideCurve::new(1.0, shape, Duration::from_secs(1));
    let duration = curve.duration();
    let left = f32::from(plot.origin.x);
    let baseline = f32::from(plot.origin.y + plot.size.height);
    let (w, h) = (f32::from(plot.size.width), f32::from(plot.size.height));
    let mut path = PathBuilder::stroke(px(2.));
    for sample in 0..=SAMPLES {
        let u = f32::from(sample) / f32::from(SAMPLES);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a speed share in 0..=1 fits an f32"
        )]
        let speed = curve.speed_at(duration.mul_f32(u)) as f32;
        let at = point(px(left + u * width * w), px(baseline - speed * top * h));
        if sample == 0 {
            path.move_to(at);
        } else {
            path.line_to(at);
        }
    }
    path.line_to(point(px(left + w), px(baseline)));
    path
}

fn paint_curve(
    bounds: Bounds<Pixels>,
    geometry: CurveGeometry,
    color: Hsla,
    axis: Hsla,
    window: &mut Window,
) {
    let inset = px(4.);
    let plot = Bounds {
        origin: point(bounds.origin.x + inset, bounds.origin.y + inset),
        size: gpui::size(
            bounds.size.width - inset * 2.,
            bounds.size.height - inset * 2.,
        ),
    };
    let baseline = plot.origin.y + plot.size.height;
    let mut floor = PathBuilder::stroke(px(1.));
    floor.move_to(point(plot.origin.x, baseline));
    floor.line_to(point(plot.origin.x + plot.size.width, baseline));
    if let Ok(floor) = floor.build() {
        window.paint_path(floor, axis);
    }
    if let Some(repeat_top) = geometry.repeat_top
        && let Ok(ghost) = curve_path(plot, geometry.shape, repeat_top, geometry.width).build()
    {
        window.paint_path(ghost, color.opacity(0.3));
    }
    if let Ok(path) = curve_path(plot, geometry.shape, geometry.top, geometry.width).build() {
        window.paint_path(path, color);
    }
}

pub(super) fn shape_name(shape: GlideShape) -> SharedString {
    match shape {
        GlideShape::Sharp => tr!("pointer.momentum_shape_sharp"),
        GlideShape::Linear => tr!("pointer.momentum_shape_linear"),
        GlideShape::Smooth => tr!("pointer.momentum_shape_smooth"),
        GlideShape::Floaty => tr!("pointer.momentum_shape_floaty"),
    }
}

fn shape_hint(shape: GlideShape) -> SharedString {
    match shape {
        GlideShape::Sharp => tr!("pointer.momentum_shape_sharp_hint"),
        GlideShape::Linear => tr!("pointer.momentum_shape_linear_hint"),
        GlideShape::Smooth => tr!("pointer.momentum_shape_smooth_hint"),
        GlideShape::Floaty => tr!("pointer.momentum_shape_floaty_hint"),
    }
}

/// One shape's tile: its curve over its name and hint, ringed in the accent
/// colour while it is the `current` shape. Clicking it picks the shape.
fn tile(
    scope: MomentumScope,
    shape: GlideShape,
    current: GlideShape,
    params: &MomentumParams,
    pal: Palette,
) -> ChoiceCard {
    let selected = shape == current;
    let accent = Hsla::from(rgb(ACCENT_BLUE));
    let color = if selected { accent } else { pal.text_primary };
    let geometry = geometry(shape, params);
    let name = shape_name(shape);
    ChoiceCard::new(
        SharedString::from(format!("glide-shape-{shape:?}")),
        name.clone(),
    )
    .selected(selected)
    .flex_1()
    .min_w_0()
    .gap_2()
    .p_3()
    .rounded(pal.card_radius)
    .border_1()
    .border_color(if selected { accent } else { pal.border })
    .cursor_pointer()
    .child(
        div().h(px(PLOT_HEIGHT)).w_full().child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    paint_curve(bounds, geometry, color, pal.border, window);
                },
            )
            .size_full(),
        ),
    )
    .child(
        v_flex()
            .child(div().text_body().text_color(pal.text_primary).child(name))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(shape_hint(shape)),
            ),
    )
    .on_click(move |_, _, cx| commit(cx, scope, |m| m.glide_shape = shape))
}

/// The picker: its label and `current` shape, what it does, and the tiles
/// drawn under `params`, laid out like the slider rows around it. A tile
/// commits its shape to `scope`'s settings.
pub(super) fn glide_shape_picker(
    scope: MomentumScope,
    current: GlideShape,
    params: &MomentumParams,
    pal: Palette,
) -> impl IntoElement {
    SliderRow::new(
        tr!("pointer.momentum_glide_shape"),
        h_flex()
            .gap_3()
            .items_stretch()
            .children(GlideShape::ALL.map(|shape| tile(scope, shape, current, params, pal))),
    )
    .value(shape_name(current), GlideShape::ALL.map(shape_name))
    .description(tr!("pointer.momentum_glide_shape_description"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(change: impl FnOnce(&mut M)) -> MomentumParams {
        let mut momentum = M::default();
        change(&mut momentum);
        momentum.params()
    }

    #[test]
    fn a_faster_max_speed_or_longer_notch_raises_the_top() {
        let base = geometry(GlideShape::Smooth, &params(|_| {}));
        let faster = geometry(
            GlideShape::Smooth,
            &params(|m| m.max_glide_speed = M::MAX_GLIDE_SPEED_MAX),
        );
        let farther = geometry(
            GlideShape::Smooth,
            &params(|m| m.pixels_per_notch = M::PIXELS_PER_NOTCH_MAX),
        );
        assert!(faster.top > base.top && farther.top > base.top);
        assert!(faster.top <= 1.0 && farther.top <= 1.0);
        let slowest = geometry(
            GlideShape::Smooth,
            &params(|m| {
                m.max_glide_speed = M::MAX_GLIDE_SPEED_MIN;
                m.pixels_per_notch = M::PIXELS_PER_NOTCH_MIN;
            }),
        );
        // Exaggerated: the slowest settings draw well under half the fastest.
        assert!(slowest.top < 0.5 * faster.top, "{slowest:?} {faster:?}");
    }

    #[test]
    fn a_longer_glide_stretches_every_shape_and_floaty_runs_longest() {
        let long = params(|m| m.glide_ms = M::GLIDE_MS_MAX);
        let widths: Vec<f32> = GlideShape::ALL
            .map(|shape| geometry(shape, &long).width)
            .to_vec();
        assert!(
            widths.windows(2).all(|pair| pair[0] < pair[1]),
            "{widths:?}"
        );
        assert!((widths[3] - 1.0).abs() < 1e-6, "{widths:?}");
        for shape in GlideShape::ALL {
            assert!(geometry(shape, &params(|_| {})).width < geometry(shape, &long).width);
        }
    }

    #[test]
    fn a_repeat_speed_up_shows_a_higher_copy_and_off_shows_none() {
        let off = geometry(GlideShape::Smooth, &params(|m| m.repeat_boost = 100));
        assert_eq!(off.repeat_top, None);
        let on = geometry(GlideShape::Smooth, &params(|_| {}));
        let repeat_top = on.repeat_top.expect("a repeat curve");
        assert!(repeat_top > on.top && repeat_top <= 1.0);
    }

    /// A hand-edited config outside the ranges draws like its nearest
    /// in-range value: core clamps it once, the graph never does.
    #[test]
    fn an_out_of_range_config_draws_inside_the_plot() {
        let wild = geometry(
            GlideShape::Floaty,
            &params(|m| {
                m.max_glide_speed = u16::MAX;
                m.pixels_per_notch = u16::MAX;
                m.glide_ms = u16::MAX;
                m.repeat_boost = u16::MAX;
            }),
        );
        assert!((0.0..=1.0).contains(&wild.top) && (0.0..=1.0).contains(&wild.width));
        assert!(wild.repeat_top.is_some_and(|top| top <= 1.0));
    }
}

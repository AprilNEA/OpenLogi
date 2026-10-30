//! What every animated ring theme moves with: the enter/exit presence, the
//! easing curves, and the path builders for rings, arcs and spokes.
//!
//! A theme keeps its own clocks and its own look; this module only lends the
//! arithmetic, so two themes never disagree on how an exit fades or how an
//! angle takes the short way round.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use gpui::{BoxShadow, Hsla, PathBuilder, Pixels, Point, Window, point, px, rgba};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::ring_style::lerp_hsla;

pub(super) const ENTER_SECS: f32 = 0.294;
/// How long the exit plays before the window goes; [`super::RingView::close`]
/// waits this long.
pub(super) const EXIT: Duration = Duration::from_millis(196);
pub(super) const EXIT_SECS: f32 = EXIT.as_secs_f32();

pub(super) const ELLIPSE_STEPS: u8 = 48;
pub(super) const ARC_STEPS: u8 = 18;
pub(super) const ARC_PIECES: u8 = 8;

/// How far in, and how visible, a ring is between its birth and its exit.
#[derive(Clone, Copy)]
pub(super) struct Presence {
    pub(super) scale: f32,
    pub(super) opacity: f32,
}

pub(super) fn presence_at(
    born: Instant,
    close_started: Option<Instant>,
    now: Instant,
    reduce: bool,
) -> Presence {
    if reduce {
        let closing = close_started.is_some();
        return Presence {
            scale: if closing { 0.86 } else { 1.0 },
            opacity: if closing { 0.0 } else { 1.0 },
        };
    }
    let enter = ease_out_cubic(now.saturating_duration_since(born).as_secs_f32() / ENTER_SECS);
    let (exit_scale, exit_opacity) = close_started.map_or((1.0, 1.0), |started| {
        let t = ease_in_cubic(now.saturating_duration_since(started).as_secs_f32() / EXIT_SECS);
        (1.0 - 0.22 * t, 1.0 - t)
    });
    Presence {
        scale: (0.62 + 0.38 * enter) * exit_scale,
        opacity: ease_out_cubic((enter * 1.6).min(1.0)) * exit_opacity,
    }
}

/// The opening: the dial grows in over this long, scale then opacity at each
/// point, running a touch past full size; the nodes follow, each starting this
/// much after the one before, gathering out from the centre to this reach and
/// scale, running a touch past their place.
const DIAL_ENTER_SECS: f32 = 0.36;
const DIAL_ENTER: [(f32, f32, f32); 3] = [(0.0, 0.18, 0.0), (0.8, 1.018, 1.0), (1.0, 1.0, 1.0)];
const NODE_ENTER_SECS: f32 = 0.26;
const NODE_ENTER_DELAY: f32 = 0.05;
const NODE_ENTER_STAGGER: f32 = 0.012;
const NODE_ENTER: [(f32, f32, f32); 3] = [(0.0, 0.0, 0.18), (0.82, 1.035, 1.015), (1.0, 1.0, 1.0)];
/// The closing, within [`EXIT`]: the dial shrinks away and the nodes gather
/// back, each a little after the one before.
const DIAL_EXIT: [(f32, f32, f32); 3] = [(0.0, 1.0, 1.0), (0.28, 0.985, 0.96), (1.0, 0.18, 0.0)];
const NODE_EXIT_SECS: f32 = 0.15;
const NODE_EXIT_STAGGER: f32 = 0.006;
const NODE_EXIT: [(f32, f32, f32); 2] = [(0.0, 1.0, 1.0), (1.0, 0.0, 0.18)];
const _: () = assert!(
    NODE_EXIT_SECS + 7.0 * NODE_EXIT_STAGGER <= EXIT_SECS,
    "the last node is gathered before the window goes"
);

/// How far a ring that opens by gathering its nodes out from the centre is
/// open this frame: the dial's scale and opacity, and each node's reach from
/// the centre, scale and opacity.
#[derive(Clone, Copy)]
pub(super) struct Stage {
    pub(super) dial: (f32, f32),
    pub(super) nodes: [(f32, f32, f32); 8],
}

/// The [`Stage`] of a ring born at `born`, closing since `close_started`.
/// Reduced motion is either fully open or, once closing, gone.
pub(super) fn stage_at(
    born: Instant,
    close_started: Option<Instant>,
    now: Instant,
    reduce: bool,
) -> Stage {
    if reduce {
        let opacity = if close_started.is_some() { 0.0 } else { 1.0 };
        return Stage {
            dial: (1.0, opacity),
            nodes: [(1.0, 1.0, opacity); 8],
        };
    }
    let since = |started: Instant| now.saturating_duration_since(started).as_secs_f32();
    let elapsed = since(born);
    let dial = keyframes(&DIAL_ENTER, elapsed / DIAL_ENTER_SECS, ease_out_cubic);
    let nodes = std::array::from_fn(|index| {
        let delay = NODE_ENTER_DELAY + NODE_ENTER_STAGGER * order(index);
        let t = (elapsed - delay) / NODE_ENTER_SECS;
        node_stage(keyframes(&NODE_ENTER, t, ease_out_cubic))
    });
    let opened = Stage { dial, nodes };
    let Some(started) = close_started else {
        return opened;
    };
    // The opening keeps running under the closing, so a ring closed part way
    // in carries on from where it got instead of jumping to full size first.
    let elapsed = since(started);
    let (scale, opacity) = keyframes(&DIAL_EXIT, elapsed / EXIT_SECS, ease_in_out_cubic);
    let (grown, shown) = opened.dial;
    let nodes = std::array::from_fn(|index| {
        let t = (elapsed - NODE_EXIT_STAGGER * order(index)) / NODE_EXIT_SECS;
        let (reach, scale, opacity) = node_stage(keyframes(&NODE_EXIT, t, ease_in_cubic));
        let (entered, grown, shown) = opened.nodes[index];
        (reach * entered, scale * grown, opacity * shown)
    });
    Stage {
        dial: (scale * grown, opacity * shown),
        nodes,
    }
}

/// A node's place in the stagger, from its index among the eight.
fn order(index: usize) -> f32 {
    u8::try_from(index).map_or(f32::MAX, f32::from)
}

/// A node's reach and scale, with the opacity that follows its reach: unseen
/// at the centre, seen in full once it is out.
fn node_stage((reach, scale): (f32, f32)) -> (f32, f32, f32) {
    (reach, scale, reach.clamp(0.0, 1.0))
}

/// How far a node `reach` of the way out sits back towards the centre, for a
/// ring whose nodes are `radius` pixels out: the gather happens inside the hit
/// box, which stays at the slot so the pointer finds it from the first frame.
pub(super) fn gather_shift(slot: ActionRingSlot, radius: f32, reach: f32) -> (f32, f32) {
    let (x, y) = slot.unit_offset();
    let back = radius * (reach - 1.0);
    (x * back, y * back)
}

/// The pair of values at `t` along `keys`, each `(at, a, b)` in order of
/// `at` from 0 to 1, with `ease` applied between the keys either side of `t`.
pub(super) fn keyframes(keys: &[(f32, f32, f32)], t: f32, ease: fn(f32) -> f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    for pair in keys.windows(2) {
        if let [(at0, a0, b0), (at1, a1, b1)] = *pair
            && t <= at1
        {
            let k = ease((t - at0) / (at1 - at0));
            return (a0 + (a1 - a0) * k, b0 + (b1 - b0) * k);
        }
    }
    keys.last().map_or((0.0, 0.0), |&(_, a, b)| (a, b))
}

/// A click presses a disc or card in for this long: at each point of it, how
/// far it sinks and how much of its size it keeps. From rest it goes in and
/// a touch smaller a third of the way in, and eases back out by three
/// quarters.
pub(super) const PRESS_SECS: f32 = 0.24;
const PRESS_KEYS: [(f32, f32, f32); 4] = [
    (0.0, 0.0, 1.0),
    (0.33, 2.5, 0.955),
    (0.75, 0.8, 0.99),
    (1.0, 0.0, 1.0),
];

/// The press that began at `started`, while it plays: how far the pressed
/// thing sinks and how much of its size it keeps.
pub(super) fn press_at(started: Instant, now: Instant) -> Option<(f32, f32)> {
    let t = now.saturating_duration_since(started).as_secs_f32() / PRESS_SECS;
    (t < 1.0).then(|| keyframes(&PRESS_KEYS, t, ease_out_cubic))
}

pub(super) fn ellipse_path(
    center: Point<Pixels>,
    radius: f32,
    stroke: Option<f32>,
) -> Option<gpui::Path<Pixels>> {
    if radius <= 1.0 {
        return None;
    }
    let mut builder = match stroke {
        Some(width) => PathBuilder::stroke(px(width)),
        None => PathBuilder::fill(),
    };
    for step in 0..=ELLIPSE_STEPS {
        let angle = f32::from(step) / f32::from(ELLIPSE_STEPS) * 2.0 * PI;
        let point = point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        );
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    builder.close();
    builder.build().ok()
}

pub(super) fn sweep_arc(
    center: Point<Pixels>,
    radius: f32,
    start: f32,
    span: f32,
    width: f32,
) -> Option<gpui::Path<Pixels>> {
    if radius <= 1.0 || width <= 0.0 {
        return None;
    }
    let mut builder = PathBuilder::stroke(px(width));
    for step in 0..=ARC_STEPS {
        let angle = start + span * (f32::from(step) / f32::from(ARC_STEPS));
        let point = point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        );
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    builder.build().ok()
}

/// An arc that fades in and out along its length: [`ARC_PIECES`] overlapping
/// strokes, each coloured by `shade` from the bell-shaped falloff (0 at the
/// ends, 1 in the middle) at its centre.
pub(super) fn paint_gradient_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    start: f32,
    span: f32,
    width: f32,
    shade: impl Fn(f32) -> Hsla,
) {
    let pieces = f32::from(ARC_PIECES);
    for step in 0..ARC_PIECES {
        let t = (f32::from(step) + 0.5) / pieces;
        let falloff = 1.0 - (2.0 * t - 1.0).powi(2);
        let piece_span = span / pieces * 1.35;
        let piece_start = start + span * (f32::from(step) / pieces);
        if let Some(path) = sweep_arc(center, radius, piece_start, piece_span, width) {
            window.paint_path(path, shade(falloff));
        }
    }
}

pub(super) fn radial_spoke(
    center: Point<Pixels>,
    inner: f32,
    outer: f32,
    angle: f32,
) -> Option<gpui::Path<Pixels>> {
    if inner <= 1.0 || outer <= inner {
        return None;
    }
    let start = point(
        center.x + px(inner * angle.cos()),
        center.y + px(inner * angle.sin()),
    );
    let end = point(
        center.x + px(outer * angle.cos()),
        center.y + px(outer * angle.sin()),
    );
    let mut builder = PathBuilder::stroke(px(1.0));
    builder.move_to(start);
    builder.line_to(end);
    builder.build().ok()
}

/// The angle from the panel's centre to `slot`, in the canvas's radians:
/// 0 is right, and y grows downwards, so Top is −π/2.
pub(super) fn slot_heading(slot: ActionRingSlot) -> f32 {
    let (x, y) = slot.unit_offset();
    y.atan2(x)
}

pub(super) fn wrap_to_pi(delta: f32) -> f32 {
    (delta + PI).rem_euclid(2.0 * PI) - PI
}

/// `current` eased towards `target` the short way round the circle.
pub(super) fn lerp_angle(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    current + wrap_to_pi(target - current) * (1.0 - (-dt / tau.max(0.001)).exp())
}

/// `#RRGGBBAA` as a colour, for the colours a theme only moves through.
pub(super) fn hex(rgba_hex: u32) -> Hsla {
    rgba(rgba_hex).into()
}

/// `rest` shadows eased `t` of the way to `hot` shadows, pair by pair, so a
/// drop shadow deepens with a lift instead of switching halfway. The lists
/// must pair up: the spread and inset come from `rest`.
pub(super) fn blend_shadows(rest: Vec<BoxShadow>, hot: Vec<BoxShadow>, t: f32) -> Vec<BoxShadow> {
    debug_assert_eq!(rest.len(), hot.len(), "shadow lists must pair up");
    rest.into_iter()
        .zip(hot)
        .map(|(rest, hot)| {
            let between = |a: Pixels, b: Pixels| a + (b - a) * t;
            let mut blended = BoxShadow::new(
                between(rest.offset.x, hot.offset.x),
                between(rest.offset.y, hot.offset.y),
                lerp_hsla(rest.color, hot.color, t),
            )
            .blur_radius(between(rest.blur_radius, hot.blur_radius))
            .spread_radius(rest.spread_radius);
            if rest.inset {
                blended = blended.inset();
            }
            blended
        })
        .collect()
}

pub(super) fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let overshoot = 1.70158;
    1.0 + (overshoot + 1.0) * (t - 1.0).powi(3) + overshoot * (t - 1.0).powi(2)
}

pub(super) fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub(super) fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// `position`, moving at `velocity`, carried `dt` towards `target` on a
/// spring that rings `hz` times a second and loses `damping` of critical
/// damping's pull: below 1 it runs a little past the target and settles,
/// and a change of target mid-way carries on from where it is. Stepped in
/// slices of at most 1/240 s so the result does not depend on the frame
/// rate.
pub(super) fn spring_towards(
    (position, velocity): (f32, f32),
    target: f32,
    dt: f32,
    hz: f32,
    damping: f32,
) -> (f32, f32) {
    const SLICE: f32 = 1.0 / 240.0;
    let omega = 2.0 * PI * hz;
    let (mut position, mut velocity) = (position, velocity);
    // A long stall, a dragged window say, plays a tenth of a second of motion
    // rather than seconds of it at once.
    let mut left = dt.min(0.1);
    while left > 0.0 {
        let step = left.min(SLICE);
        let pull = omega * omega * (target - position) - 2.0 * damping * omega * velocity;
        velocity += pull * step;
        position += velocity * step;
        left -= step;
    }
    (position, velocity)
}

/// `current` eased towards `target` with time constant `tau`, frame-rate
/// independent: it covers the same fraction of the remaining distance per
/// second whatever `dt` is.
pub(super) fn smooth_towards(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    let t = 1.0 - (-dt / tau.max(0.001)).exp();
    current + (target - current) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_towards_approaches_without_overshoot() {
        let mut value = 0.0;
        for _ in 0..40 {
            value = smooth_towards(value, 1.0, 1.0 / 60.0, 0.09);
        }
        assert!(value > 0.95);
        assert!(value <= 1.0);
        let next = smooth_towards(value, 1.0, 1.0 / 60.0, 0.09);
        assert!(next >= value);
        assert!(next <= 1.0);
    }

    #[test]
    fn presence_expands_from_the_centre_then_holds() {
        let born = Instant::now();
        let start = presence_at(born, None, born, false);
        assert!(start.scale < 0.7);
        assert!(start.opacity < 0.2);
        let settled = presence_at(born, None, born + Duration::from_millis(500), false);
        assert!((settled.scale - 1.0).abs() < 0.02);
        assert!((settled.opacity - 1.0).abs() < 0.02);
        let closed = presence_at(born, Some(born), born + Duration::from_millis(300), false);
        assert!(closed.opacity < 0.05);
        assert!(closed.scale < 0.9);
    }

    #[test]
    fn wrap_to_pi_picks_the_short_turn() {
        assert!((wrap_to_pi(0.2) - 0.2).abs() < 0.001);
        let delta = wrap_to_pi(0.2 - 6.0);
        assert!(delta > 0.0);
        assert!(delta < PI);
        assert!(wrap_to_pi(PI + 0.2).abs() < PI);
    }

    #[test]
    fn lerp_angle_chases_without_the_long_way_around() {
        let mut angle = 6.0;
        for _ in 0..48 {
            angle = lerp_angle(angle, 0.2, 1.0 / 60.0, 0.13);
        }
        assert!(wrap_to_pi(angle - 0.2).abs() < 0.05);
    }

    #[test]
    fn closing_part_way_in_carries_on_from_where_the_opening_got() {
        let born = Instant::now();
        let closed_at = born + Duration::from_millis(120);
        let before = stage_at(born, None, closed_at, false);
        assert!(
            before.dial.0 < 0.9,
            "still opening at 120 ms: {}",
            before.dial.0
        );
        let after = stage_at(born, Some(closed_at), closed_at, false);
        assert!(
            (after.dial.0 - before.dial.0).abs() < 0.001,
            "the dial holds its size"
        );
        assert!((after.dial.1 - before.dial.1).abs() < 0.001);
        for (index, (reach, scale, opacity)) in after.nodes.into_iter().enumerate() {
            let (entered, grown, shown) = before.nodes[index];
            assert!(
                (reach - entered).abs() < 0.001,
                "node {index} holds its reach"
            );
            assert!((scale - grown).abs() < 0.001 && (opacity - shown).abs() < 0.001);
        }
        let gone = stage_at(born, Some(closed_at), closed_at + EXIT, false);
        assert!(gone.dial.1 < 0.001, "gone when the window goes");
        assert!(
            gone.nodes
                .iter()
                .all(|&(reach, _, opacity)| reach < 0.001 && opacity < 0.001)
        );
    }

    #[test]
    fn spring_runs_a_little_past_then_settles_whatever_the_frame_rate() {
        let run = |frames: u32, dt: f32| {
            let mut state = (0.0, 0.0);
            let mut peak: f32 = 0.0;
            for _ in 0..frames {
                state = spring_towards(state, 1.0, dt, 3.2, 0.6);
                peak = peak.max(state.0);
            }
            (state.0, peak)
        };
        let (at_60, peak_60) = run(60, 1.0 / 60.0);
        let (at_144, peak_144) = run(144, 1.0 / 144.0);
        assert!(peak_60 > 1.02 && peak_60 < 1.15, "overshoot {peak_60}");
        assert!((at_60 - 1.0).abs() < 0.01, "settled at {at_60}");
        assert!((peak_60 - peak_144).abs() < 0.005);
        assert!((at_60 - at_144).abs() < 0.005);
    }

    #[test]
    fn hover_bubble_grows_from_nothing() {
        assert!(ease_out_back(0.0).abs() < 0.001);
        assert!((ease_out_back(1.0) - 1.0).abs() < 0.001);
        assert!(ease_out_back(0.8) > 1.0);
    }
}

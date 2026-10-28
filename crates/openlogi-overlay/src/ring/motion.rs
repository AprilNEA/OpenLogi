//! What every animated ring theme moves with: the enter/exit presence, the
//! easing curves, and the path builders for rings, arcs and spokes.
//!
//! A theme keeps its own clocks and its own look; this module only lends the
//! arithmetic, so two themes never disagree on how an exit fades or how an
//! angle takes the short way round.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use gpui::{Hsla, PathBuilder, Pixels, Point, Window, point, px};
use openlogi_core::binding::ActionRingSlot;

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
    fn hover_bubble_grows_from_nothing() {
        assert!(ease_out_back(0.0).abs() < 0.001);
        assert!((ease_out_back(1.0) - 1.0).abs() < 0.001);
        assert!(ease_out_back(0.8) > 1.0);
    }
}

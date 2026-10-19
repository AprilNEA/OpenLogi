//! Painted look of the Actions Ring: pill slots with captions, the HUD rings
//! and arcs around them, and the enter/exit motion.
//!
//! Placement, click-away, and IPC stay in [`super`]; this module only decides
//! what the panel looks like and how it moves, so the two change independently.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use gpui::{
    Background, Context, Hsla, InteractiveElement, IntoElement, ParentElement, PathBuilder, Pixels,
    Point, SharedString, StatefulInteractiveElement as _, Styled, Window, canvas, div,
    linear_color_stop, linear_gradient, point, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingPresentation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;

use super::{RingView, WINDOW_SIZE};
use crate::ipc::OverlayCommand;
use crate::session;

const PILL_W: f32 = 132.0;
const PILL_H: f32 = 34.0;
const HOVER_RING: f32 = 64.0;
const RADIUS: f32 = 190.0;
const ENTER_SECS: f32 = 0.294;
const EXIT_SECS: f32 = 0.196;
const HOVER_TAU: f32 = 0.09;
const ARC_SPAN: f32 = 0.70;
const ARC_SPIN: f32 = 0.55;
const ARC_TAU: f32 = 0.13;
const ARC_PIECES: u8 = 8;
const EXIT: Duration = Duration::from_millis(196);

const GLYPH: Hsla = neutral(0.98, 1.0);
const INK: Hsla = neutral(0.08, 1.0);
const CANCEL_GLYPH: Hsla = neutral(0.84, 1.0);

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// Hover, arc, and enter/exit clocks for one open ring.
pub(crate) struct Motion {
    hover_t: [f32; 8],
    arc_angle: f32,
    born: Instant,
    last_tick: Instant,
    close_started: Option<Instant>,
}

impl Default for Motion {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            hover_t: [0.0; 8],
            arc_angle: 0.0,
            born: now,
            last_tick: now,
            close_started: None,
        }
    }
}

impl Motion {
    /// Start the exit. Returns false when the exit is already playing.
    fn begin_close(&mut self, now: Instant) -> bool {
        if self.close_started.is_some() {
            return false;
        }
        self.close_started = Some(now);
        true
    }

    fn advance(&mut self, hovered: Option<ActionRingSlot>, now: Instant, reduce: bool) {
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        for slot in ActionRingSlot::ALL {
            let target = if hovered == Some(slot) { 1.0 } else { 0.0 };
            let index = slot.index();
            self.hover_t[index] = if reduce {
                target
            } else {
                smooth_towards(self.hover_t[index], target, dt, HOVER_TAU)
            };
        }
        let heading = hovered.map(slot_heading);
        self.arc_angle = if reduce {
            heading.unwrap_or(self.arc_angle)
        } else if let Some(heading) = heading {
            lerp_angle(self.arc_angle, heading, dt, ARC_TAU)
        } else {
            self.arc_angle + ARC_SPIN * dt
        };
    }

    fn presence(&self, now: Instant, reduce: bool) -> Presence {
        presence_at(self.born, self.close_started, now, reduce)
    }

    fn elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }
}

impl RingView {
    /// Whether the exit animation is already playing.
    pub(crate) const fn is_closing(&self) -> bool {
        self.skin.close_started.is_some()
    }

    /// Send the ring's final command once, play the bubble-out, then close
    /// this window. Other windows of the session close right away.
    pub(super) fn close(&mut self, command: OverlayCommand, cx: &mut Context<Self>) {
        if !self.skin.begin_close(Instant::now()) {
            return;
        }
        let _ = self.commands.send(command);
        let session_id = self.invocation.session_id;
        session::close_ring_windows(cx, session_id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(EXIT).await;
            let _ = this.update(cx, |_, cx| {
                let id = cx.entity_id();
                cx.with_window(id, |window, cx| {
                    session::dismiss_from_window(cx, window, session_id);
                });
            });
        })
        .detach();
    }

    /// The fixed square panel both placements anchor. It takes the background
    /// click itself so the stock root handler never closes without the exit.
    pub(super) fn skinned_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let reduce = cx.reduce_motion();
        self.skin.advance(self.hovered, now, reduce);
        let presence = self.skin.presence(now, reduce);
        let elapsed = self.skin.elapsed(now);
        let slots = ActionRingSlot::ALL
            .into_iter()
            .zip(self.skin.hover_t)
            .filter_map(|(slot, hover_t)| slot_element(self, slot, hover_t, cx))
            .collect::<Vec<_>>();

        div()
            .id("ring-skin")
            .relative()
            .size(px(WINDOW_SIZE))
            .opacity(presence.opacity)
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                let session_id = this.invocation.session_id;
                this.close(OverlayCommand::Cancel { session_id }, cx);
            }))
            .child(ambient_field(
                elapsed,
                presence.scale,
                self.skin.hover_t,
                self.skin.arc_angle,
                reduce,
            ))
            .children(slots)
            .child(cancel_control(cx))
    }
}

fn slot_element(
    view: &RingView,
    slot: ActionRingSlot,
    hover_t: f32,
    cx: &mut Context<RingView>,
) -> Option<gpui::AnyElement> {
    let presentation = view.invocation.slots.get(&slot)?;
    let icon_path = presentation.icon.asset_path();
    let caption = slot_caption(presentation);
    // Pills sit at their final radius from the first frame: the ambient field
    // bubbles in and out around them, but a target the user is already moving
    // towards must not shift under the pointer, and a pointer that lands on a
    // slot while the ring is still opening must hover it.
    let (left, top) = slot_origin(slot, WINDOW_SIZE, RADIUS);
    let session_id = view.invocation.session_id;
    let ink = lerp_hsla(GLYPH, INK, hover_t);
    Some(
        div()
            .id(("ring-slot", slot.index()))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(PILL_W))
            .h(px(PILL_H))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .rounded_full()
            .bg(slot_fill(hover_t))
            .text_color(ink)
            .cursor_pointer()
            .child(
                svg()
                    .path(icon_path)
                    .size(px(16.0))
                    .flex_none()
                    .text_color(ink),
            )
            .child(div().w(px(92.0)).text_xs().truncate().child(caption))
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                if this.is_closing() {
                    return;
                }
                if *hovered && this.hovered != Some(slot) {
                    this.hovered = Some(slot);
                    let _ = this
                        .commands
                        .send(OverlayCommand::Hover { session_id, slot });
                    cx.notify();
                } else if !*hovered && this.hovered == Some(slot) {
                    this.hovered = None;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.close(OverlayCommand::Activate { session_id, slot }, cx);
            }))
            .into_any_element(),
    )
}

fn cancel_control(cx: &mut Context<RingView>) -> impl IntoElement {
    div()
        .id("ring-cancel")
        .absolute()
        .left(px(WINDOW_SIZE / 2.0 - 24.0))
        .top(px(WINDOW_SIZE / 2.0 - 24.0))
        .size(px(48.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(linear_gradient(
            145.0,
            linear_color_stop(color::accent().alpha(0.28), 0.0),
            linear_color_stop(color::accent().alpha(0.06), 1.0),
        ))
        .text_color(CANCEL_GLYPH)
        .cursor_pointer()
        .hover(|cancel| {
            cancel
                .bg(linear_gradient(
                    145.0,
                    linear_color_stop(color::accent_at_lightness(0.52), 0.0),
                    linear_color_stop(color::accent_at_lightness(0.28), 1.0),
                ))
                .text_color(GLYPH)
        })
        .child(svg().path(RING_CANCEL_ICON).size(px(20.0)).flex_none())
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            let session_id = this.invocation.session_id;
            this.close(OverlayCommand::Cancel { session_id }, cx);
        }))
}

#[derive(Clone, Copy)]
struct Presence {
    scale: f32,
    opacity: f32,
}

fn presence_at(
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

fn slot_fill(hover_t: f32) -> Background {
    let rest_top = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.18,
        a: 0.92,
    };
    let rest_bot = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.10,
        a: 0.92,
    };
    let hot_top = Hsla {
        h: 0.0,
        s: 0.0,
        l: 1.0,
        a: 1.0,
    };
    let hot_bot = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.86,
        a: 1.0,
    };
    linear_gradient(
        160.0,
        linear_color_stop(lerp_hsla(rest_top, hot_top, hover_t), 0.0),
        linear_color_stop(lerp_hsla(rest_bot, hot_bot, hover_t), 1.0),
    )
}

const fn lerp_hsla(a: Hsla, b: Hsla, t: f32) -> Hsla {
    Hsla {
        h: a.h + (b.h - a.h) * t,
        s: a.s + (b.s - a.s) * t,
        l: a.l + (b.l - a.l) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn slot_caption(presentation: &ActionRingPresentation) -> SharedString {
    // User-authored labels render verbatim: passing them through the
    // localization table would translate any label that happens to collide
    // with a known key ("Copy" would become "Copier" under fr).
    let label = if presentation.literal {
        presentation.label.clone()
    } else if let Some(key) = Action::translation_key_for_label(&presentation.label) {
        rust_i18n::t!(key).into_owned()
    } else {
        presentation.label.clone()
    };
    SharedString::from(label)
}

/// Centre of `slot`'s pill within the panel, for tests that aim the pointer at it.
#[cfg(test)]
pub(super) fn slot_center(slot: ActionRingSlot) -> (f32, f32) {
    let (left, top) = slot_origin(slot, WINDOW_SIZE, RADIUS);
    (left + PILL_W / 2.0, top + PILL_H / 2.0)
}

fn slot_origin(slot: ActionRingSlot, canvas: f32, radius: f32) -> (f32, f32) {
    let (x, y) = slot.unit_offset();
    (
        canvas / 2.0 + x * radius - PILL_W / 2.0,
        canvas / 2.0 + y * radius - PILL_H / 2.0,
    )
}

fn ambient_field(
    elapsed: f32,
    scale: f32,
    hover_t: [f32; 8],
    arc_angle: f32,
    reduce: bool,
) -> impl IntoElement {
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            if !reduce {
                window.request_animation_frame();
            }
            let ambient = Ambient {
                elapsed,
                scale,
                hover_t,
                arc_angle,
                reduce,
            };
            paint_ambient(window, bounds, &ambient);
        },
    )
    .absolute()
    .size_full()
}

struct Ambient {
    elapsed: f32,
    scale: f32,
    hover_t: [f32; 8],
    arc_angle: f32,
    reduce: bool,
}

fn paint_ambient(window: &mut Window, bounds: gpui::Bounds<Pixels>, ambient: &Ambient) {
    let center = bounds.center();
    let breath = if ambient.reduce {
        1.0
    } else {
        0.5 + 0.5 * (ambient.elapsed * 1.15).sin()
    };
    let parked = ambient.hover_t.iter().copied().fold(0.0_f32, f32::max);
    paint_hub_glow(window, center, ambient.scale, breath);
    paint_hud_tracks(
        window,
        center,
        ambient.scale,
        breath,
        HudMotion {
            elapsed: ambient.elapsed,
            arc_angle: ambient.arc_angle,
            parked,
            reduce: ambient.reduce,
        },
    );
    for (slot, amount) in ActionRingSlot::ALL.into_iter().zip(ambient.hover_t) {
        if amount >= 0.02 {
            let (ux, uy) = slot.unit_offset();
            paint_hover_bubble(
                window,
                center,
                ambient.scale,
                ambient.elapsed,
                (ux, uy, amount),
                ambient.reduce,
            );
        }
    }
}

fn paint_hub_glow(window: &mut Window, center: Point<Pixels>, scale: f32, breath: f32) {
    for (radius, alpha) in [
        (72.0 * scale, 0.12 + 0.05 * breath),
        (48.0 * scale, 0.16 + 0.06 * breath),
        (28.0 * scale, 0.22),
    ] {
        if let Some(path) = ellipse_path(center, radius, None) {
            window.paint_path(path, accent_wash(145.0, alpha));
        }
    }
}

fn paint_hud_tracks(
    window: &mut Window,
    center: Point<Pixels>,
    scale: f32,
    breath: f32,
    motion: HudMotion,
) {
    let track = RADIUS * scale;
    let inner = track * 0.62;
    let outer = track * 1.16;
    let grid = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.58,
        a: 0.10 + 0.04 * breath,
    };
    for radius in [inner, track, outer] {
        if let Some(path) = ellipse_path(center, radius, Some(1.0)) {
            window.paint_path(path, grid);
        }
    }
    for slot in ActionRingSlot::ALL {
        let (x, y) = slot.unit_offset();
        if let Some(path) = radial_spoke(center, inner, outer, y.atan2(x)) {
            window.paint_path(path, grid.alpha(grid.a * 0.7));
        }
    }
    paint_gradient_arc(
        window,
        center,
        track,
        motion.arc_angle - ARC_SPAN / 2.0,
        ARC_SPAN,
        0.34 + 0.50 * motion.parked,
        2.8,
    );
    if !motion.reduce {
        let drift = motion.elapsed * ARC_SPIN;
        let ambient = (1.0 - 0.78 * motion.parked).max(0.08);
        paint_gradient_arc(
            window,
            center,
            inner,
            drift + 1.15,
            0.55,
            0.22 * ambient,
            2.0,
        );
        paint_gradient_arc(
            window,
            center,
            outer,
            drift + 2.30,
            0.48,
            0.16 * ambient,
            1.8,
        );
    }
}

#[derive(Clone, Copy)]
struct HudMotion {
    elapsed: f32,
    arc_angle: f32,
    parked: f32,
    reduce: bool,
}

fn paint_hover_bubble(
    window: &mut Window,
    center: Point<Pixels>,
    scale: f32,
    elapsed: f32,
    focus: (f32, f32, f32),
    reduce: bool,
) {
    let (ux, uy, amount) = focus;
    let grow = if reduce { 1.0 } else { ease_out_back(amount) };
    let pulse = if reduce {
        0.0
    } else {
        (elapsed * 5.2).sin() * 0.05 * amount
    };
    let size = (grow + pulse).max(0.0);
    if size < 0.04 {
        return;
    }
    let slot = point(
        center.x + px(ux * RADIUS * scale),
        center.y + px(uy * RADIUS * scale),
    );
    let heading = uy.atan2(ux).to_degrees() + 90.0;
    if let Some(path) = ellipse_path(slot, 52.0 * scale * size, None) {
        window.paint_path(path, accent_wash(heading, 0.34 * amount));
    }
    let rings = [
        (HOVER_RING * 0.5, 0.90, 0.0),
        (HOVER_RING * 0.5 + 8.0, 0.34, 0.12),
        (HOVER_RING * 0.5 + 16.0, 0.18, 0.22),
    ];
    for (radius, alpha, delay) in rings {
        let ring_t = delayed(amount, delay);
        if ring_t < 0.02 {
            continue;
        }
        let ring_size = if reduce { 1.0 } else { ease_out_back(ring_t) };
        if let Some(path) = ellipse_path(slot, radius * scale * ring_size, Some(1.35)) {
            window.paint_path(path, accent_wash(heading, alpha * ring_t));
        }
    }
}

const ELLIPSE_STEPS: u8 = 48;
const ARC_STEPS: u8 = 18;

fn ellipse_path(
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

fn sweep_arc(
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

fn paint_gradient_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    start: f32,
    span: f32,
    peak_alpha: f32,
    width: f32,
) {
    if peak_alpha < 0.01 {
        return;
    }
    let pieces = f32::from(ARC_PIECES);
    for step in 0..ARC_PIECES {
        let t = (f32::from(step) + 0.5) / pieces;
        let falloff = 1.0 - (2.0 * t - 1.0).powi(2);
        let piece_span = span / pieces * 1.35;
        let piece_start = start + span * (f32::from(step) / pieces);
        if let Some(path) = sweep_arc(center, radius, piece_start, piece_span, width) {
            window.paint_path(
                path,
                color::accent_at_lightness(0.68 + 0.14 * falloff).alpha(peak_alpha * falloff),
            );
        }
    }
}

fn accent_wash(angle_deg: f32, alpha: f32) -> Background {
    linear_gradient(
        angle_deg,
        linear_color_stop(color::accent_at_lightness(0.82).alpha(alpha), 0.0),
        linear_color_stop(color::accent().alpha(0.0), 1.0),
    )
}

fn delayed(amount: f32, delay: f32) -> f32 {
    if delay >= 1.0 {
        0.0
    } else {
        ((amount - delay) / (1.0 - delay)).clamp(0.0, 1.0)
    }
}

fn slot_heading(slot: ActionRingSlot) -> f32 {
    let (x, y) = slot.unit_offset();
    y.atan2(x)
}

fn wrap_to_pi(delta: f32) -> f32 {
    (delta + PI).rem_euclid(2.0 * PI) - PI
}

fn lerp_angle(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    current + wrap_to_pi(target - current) * (1.0 - (-dt / tau.max(0.001)).exp())
}

fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let overshoot = 1.70158;
    1.0 + (overshoot + 1.0) * (t - 1.0).powi(3) + overshoot * (t - 1.0).powi(2)
}

fn radial_spoke(
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

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

fn smooth_towards(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
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
            value = smooth_towards(value, 1.0, 1.0 / 60.0, HOVER_TAU);
        }
        assert!(value > 0.95);
        assert!(value <= 1.0);
        let next = smooth_towards(value, 1.0, 1.0 / 60.0, HOVER_TAU);
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
    fn command_slots_are_pills_that_overflow_the_hover_ring() {
        const { assert!(PILL_W > HOVER_RING) };
        const { assert!(PILL_H < HOVER_RING) };
        let origin = slot_origin(ActionRingSlot::Right, WINDOW_SIZE, RADIUS);
        let (cx, cy) = (origin.0 + PILL_W / 2.0, origin.1 + PILL_H / 2.0);
        assert!((cx - (WINDOW_SIZE / 2.0 + RADIUS)).abs() < 0.01);
        assert!((cy - WINDOW_SIZE / 2.0).abs() < 0.01);
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
            angle = lerp_angle(angle, 0.2, 1.0 / 60.0, ARC_TAU);
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

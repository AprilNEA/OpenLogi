//! Painted look of the Actions Ring: pill slots with captions, the HUD rings
//! and arcs around them, and the enter/exit motion.
//!
//! Placement, click-away, and IPC stay in [`super`]; this module only decides
//! what the panel looks like and how it moves, so the two change independently.

use std::time::Instant;

use gpui::{
    Background, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    SharedString, StatefulInteractiveElement as _, Styled, Window, canvas, div, linear_color_stop,
    linear_gradient, point, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
pub(super) use openlogi_ui::ring_style::in_motion::DESIGN_EDGE;
use openlogi_ui::ring_style::in_motion::{
    CANCEL_GLYPH, GLYPH, HUB, HUB_GLOW, HUB_GLYPH, PILL_GLYPH, PILL_H, PILL_W, RADIUS, TRACK_INNER,
    TRACK_OUTER, hub_fill, hub_fill_hovered, slot_fill, slot_ink, track,
};

use super::RingView;
use super::motion::{
    Presence, ease_out_back, ease_out_cubic, ellipse_path, lerp_angle, presence_at, radial_spoke,
    slot_heading, smooth_towards,
};
use crate::ipc::OverlayCommand;

/// The hub open: the circle that carries the hovered slot's full caption.
/// Clears every pill at the 160 px slot radius.
const HUB_OPEN: f32 = 136.0;
/// How fast the hub opens and closes.
const HUB_TAU: f32 = 0.12;
const HOVER_RING: f32 = 64.0;
const HOVER_TAU: f32 = 0.09;
const ARC_SPAN: f32 = 0.70;
const ARC_SPIN: f32 = 0.55;
const ARC_TAU: f32 = 0.13;

/// Hover, arc, and enter/exit clocks for one open ring.
pub(super) struct Motion {
    hover_t: [f32; 8],
    /// 0 with the hub at rest, 1 fully open around a caption.
    hub_t: f32,
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
            hub_t: 0.0,
            arc_angle: 0.0,
            born: now,
            last_tick: now,
            close_started: None,
        }
    }
}

impl Motion {
    /// Whether the exit is already playing.
    pub(super) const fn is_closing(&self) -> bool {
        self.close_started.is_some()
    }

    /// Start the exit. Returns false when the exit is already playing.
    pub(super) fn begin_close(&mut self, now: Instant) -> bool {
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
        let hub_target = if hovered.is_some() { 1.0 } else { 0.0 };
        self.hub_t = if reduce {
            hub_target
        } else {
            smooth_towards(self.hub_t, hub_target, dt, HUB_TAU)
        };
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

    /// Advance the clocks to `now` and copy out what this frame draws.
    pub(super) fn frame(
        &mut self,
        hovered: Option<ActionRingSlot>,
        now: Instant,
        reduce: bool,
    ) -> Frame {
        self.advance(hovered, now, reduce);
        Frame {
            hover_t: self.hover_t,
            arc_angle: self.arc_angle,
            hub_t: self.hub_t,
            presence: self.presence(now, reduce),
            elapsed: self.elapsed(now),
            reduce,
        }
    }
}

/// What one frame of the panel is drawn from: the clocks as they stand,
/// copied out of the motion so the panel borrows nothing.
#[derive(Clone, Copy)]
pub(super) struct Frame {
    hover_t: [f32; 8],
    arc_angle: f32,
    hub_t: f32,
    presence: Presence,
    elapsed: f32,
    reduce: bool,
}

impl RingView {
    /// The panel, `edge` pixels square (see [`super::panel_edge`]). Below
    /// [`DESIGN_EDGE`] the whole layout scales down with it: slot radius,
    /// pills, hub and the ambient field alike. It takes the background click
    /// itself so the stock root handler never closes without the exit.
    pub(super) fn skinned_panel(
        &self,
        frame: Frame,
        edge: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            hover_t,
            arc_angle,
            hub_t,
            presence,
            elapsed,
            reduce,
        } = frame;
        let layout = Layout::fitting(edge);
        let slots = ActionRingSlot::ALL
            .into_iter()
            .zip(hover_t)
            .filter_map(|(slot, hover_t)| slot_element(self, slot, hover_t, layout, cx))
            .collect::<Vec<_>>();
        let hovered_caption = self.hovered_caption();

        div()
            .id("ring-skin")
            .relative()
            .size(px(layout.edge))
            .opacity(presence.opacity)
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                let session_id = this.invocation.session_id;
                this.close(OverlayCommand::Cancel { session_id }, window, cx);
            }))
            .child(ambient_field(
                elapsed,
                presence.scale * layout.fit,
                hover_t,
                arc_angle,
                reduce,
            ))
            .children(slots)
            .child(cancel_control(hovered_caption, hub_t, layout, cx))
    }
}

/// The panel's edge and how far the layout is scaled down to fit it. `fit`
/// is 1 on every display at least [`DESIGN_EDGE`] tall and wide, so the
/// geometry below is exactly the designed one there.
#[derive(Clone, Copy)]
struct Layout {
    edge: f32,
    fit: f32,
}

impl Layout {
    fn fitting(edge: Pixels) -> Self {
        let edge = f32::from(edge).min(DESIGN_EDGE);
        Self {
            edge,
            fit: edge / DESIGN_EDGE,
        }
    }

    /// The designed geometry, for tests that aim at it.
    #[cfg(test)]
    const fn full() -> Self {
        Self {
            edge: DESIGN_EDGE,
            fit: 1.0,
        }
    }

    /// Top-left of `slot`'s pill.
    fn slot_origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        (
            self.edge / 2.0 + x * RADIUS * self.fit - self.pill_width() / 2.0,
            self.edge / 2.0 + y * RADIUS * self.fit - self.pill_height() / 2.0,
        )
    }

    /// The pill scales with the slot radius in both directions: a shorter
    /// panel brings the slots closer, and a pill that kept its designed height
    /// would overlap its neighbours below 320 px, so a click near a shared
    /// corner could reach the wrong action.
    fn pill_width(self) -> f32 {
        PILL_W * self.fit
    }

    fn pill_height(self) -> f32 {
        PILL_H * self.fit
    }
}

fn slot_element(
    view: &RingView,
    slot: ActionRingSlot,
    hover_t: f32,
    layout: Layout,
    cx: &mut Context<RingView>,
) -> Option<gpui::AnyElement> {
    let presentation = view.invocation.slots.get(&slot)?;
    let icon_path = presentation.icon.asset_path();
    let caption = super::slot_caption(presentation);
    // Pills sit at their final radius from the first frame: the ambient field
    // bubbles in and out around them, but a target the user is already moving
    // towards must not shift under the pointer, and a pointer that lands on a
    // slot while the ring is still opening must hover it.
    let (left, top) = layout.slot_origin(slot);
    let session_id = view.invocation.session_id;
    let ink = slot_ink(hover_t);
    Some(
        div()
            .id(("ring-slot", slot.index()))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(layout.pill_width()))
            .h(px(layout.pill_height()))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0 * layout.fit))
            .px(px(12.0 * layout.fit))
            .rounded_full()
            .bg(slot_fill(hover_t))
            .text_color(ink)
            .cursor_pointer()
            .child(
                svg()
                    .path(icon_path)
                    .size(px(PILL_GLYPH))
                    .flex_none()
                    .text_color(ink),
            )
            .child(
                div()
                    .w(px(layout.pill_width() - 40.0 * layout.fit))
                    .text_xs()
                    .truncate()
                    .child(caption),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                this.hover(slot, *hovered, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close(OverlayCommand::Activate { session_id, slot }, window, cx);
            }))
            .into_any_element(),
    )
}

/// The hub: the cancel button, which opens into a larger circle carrying the
/// hovered slot's whole caption and closes again when nothing is hovered.
/// A pill truncates a long label, and user-authored labels can differ only
/// at their end, so hovering must still show all of it; the hub is the one
/// spot every slot is equidistant from. `hub_t` is how far open it is.
fn cancel_control(
    caption: Option<SharedString>,
    hub_t: f32,
    layout: Layout,
    cx: &mut Context<RingView>,
) -> impl IntoElement {
    let open = ease_out_cubic(hub_t);
    let size = (HUB + (HUB_OPEN - HUB) * open) * layout.fit;
    div()
        .id("ring-cancel")
        .absolute()
        .left(px(layout.edge / 2.0 - size / 2.0))
        .top(px(layout.edge / 2.0 - size / 2.0))
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(hub_fill())
        .text_color(CANCEL_GLYPH)
        .cursor_pointer()
        .hover(|cancel| cancel.bg(hub_fill_hovered()).text_color(GLYPH))
        .map(|hub| match caption {
            // The caption fades in as the circle opens; the cross fades back
            // as it closes, so neither shows at a size it does not fit.
            Some(caption) => hub.child(
                div()
                    .w(px((HUB_OPEN - 28.0) * layout.fit))
                    .text_sm()
                    .text_center()
                    .text_color(GLYPH)
                    .opacity(open)
                    .child(caption),
            ),
            None => hub.child(
                svg()
                    .path(RING_CANCEL_ICON)
                    .size(px(HUB_GLYPH))
                    .flex_none()
                    .opacity(1.0 - open),
            ),
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            let session_id = this.invocation.session_id;
            this.close(OverlayCommand::Cancel { session_id }, window, cx);
        }))
}

/// Centre of `slot`'s pill within a full-size panel, for tests that aim the
/// pointer at it.
#[cfg(test)]
pub(super) fn slot_center(slot: ActionRingSlot) -> (f32, f32) {
    let (left, top) = Layout::full().slot_origin(slot);
    (left + PILL_W / 2.0, top + PILL_H / 2.0)
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
    for (radius, alpha, swing) in HUB_GLOW {
        let (radius, alpha) = (radius * scale, alpha + swing * breath);
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
    let inner = track * TRACK_INNER;
    let outer = track * TRACK_OUTER;
    let grid = self::track(breath);
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

/// An accent arc that fades in and out along its length, `peak_alpha` at
/// its middle; nothing when it would be invisible.
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
    super::motion::paint_gradient_arc(window, center, radius, start, span, width, |falloff| {
        color::accent_at_lightness(0.68 + 0.14 * falloff).alpha(peak_alpha * falloff)
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_slots_are_pills_that_overflow_the_hover_ring() {
        const { assert!(PILL_W > HOVER_RING) };
        const { assert!(PILL_H < HOVER_RING) };
        let origin = Layout::full().slot_origin(ActionRingSlot::Right);
        let (cx, cy) = (origin.0 + PILL_W / 2.0, origin.1 + PILL_H / 2.0);
        assert!((cx - (DESIGN_EDGE / 2.0 + RADIUS)).abs() < 0.01);
        assert!((cy - DESIGN_EDGE / 2.0).abs() < 0.01);
    }

    #[test]
    fn a_short_panel_scales_the_slot_ring_and_keeps_every_pill_inside() {
        // 1280×720 at 200%: a 360 px panel, three quarters of the design.
        let layout = Layout::fitting(px(360.0));
        assert!((layout.fit - 0.75).abs() < 0.001);
        let full = Layout::fitting(px(1080.0));
        assert!(
            (full.fit - 1.0).abs() < 0.001,
            "a tall display keeps the design size"
        );
        for slot in ActionRingSlot::ALL {
            let (left, top) = layout.slot_origin(slot);
            assert!(
                left >= 0.0 && left + layout.pill_width() <= layout.edge,
                "{slot:?}"
            );
            assert!(
                top >= 0.0 && top + layout.pill_height() <= layout.edge,
                "{slot:?}"
            );
        }
        let (left, top) = layout.slot_origin(ActionRingSlot::Bottom);
        assert!((top + layout.pill_height() / 2.0 - (180.0 + 120.0)).abs() < 0.01);
        assert!((left + layout.pill_width() / 2.0 - 180.0).abs() < 0.01);
    }

    #[test]
    fn neighbouring_pills_never_overlap_however_short_the_panel() {
        // 1024×600 at 200% is a 300 px panel: at the designed pill height the
        // Top and TopRight pills shared a 12×5 px corner, so a click there
        // could pick either. 240 px is below any display GPUI would offer and
        // stands in for the limit.
        for edge in [480.0, 360.0, 300.0, 240.0] {
            let layout = Layout::fitting(px(edge));
            let rect = |slot: ActionRingSlot| {
                let (left, top) = layout.slot_origin(slot);
                (
                    left,
                    top,
                    left + layout.pill_width(),
                    top + layout.pill_height(),
                )
            };
            for (index, slot) in ActionRingSlot::ALL.into_iter().enumerate() {
                let next = ActionRingSlot::ALL[(index + 1) % ActionRingSlot::ALL.len()];
                let (a, b) = (rect(slot), rect(next));
                let apart = a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1;
                assert!(apart, "{slot:?} and {next:?} overlap on a {edge} px panel");
            }
        }
    }
}

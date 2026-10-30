//! The `Porcelain` theme: ceramic discs with their captions under them, on
//! one glazed platter that holds the whole ring, around a hub that names the
//! hovered action and closes the ring. A terracotta needle inside the
//! platter's inset ring turns to point at the hovered disc, the disc presses
//! in under the pointer as its caption warms, a click presses it further and
//! draws a short stroke under the caption, and the ring opens by gathering
//! its nodes out from the centre and closes by gathering them back.
//!
//! The look at rest is [`openlogi_ui::ring_style::porcelain`], shared with
//! the settings app's preview; everything that moves is here.

use std::f32::consts::PI;
use std::time::Instant;

use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement, PathBuilder, Pixels,
    Point, StatefulInteractiveElement as _, Styled, canvas, div, point,
    prelude::FluentBuilder as _, px,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::ring_style::lerp_hsla;
pub(super) use openlogi_ui::ring_style::porcelain::DESIGN_EDGE;
use openlogi_ui::ring_style::porcelain::{
    DISC, DISC_GLYPH, HUB, HUB_GAP, HUB_GLYPH, HUB_LABEL, INSET_RING, LABEL, MARK_H, MARK_W,
    NEEDLE_INNER, NEEDLE_OUTER, NEEDLE_W, NODE_GAP, NODE_H, NODE_PAD_TOP, NODE_W, PLATTER, RADIUS,
    accent, close_ink, disc_border, disc_border_hovered, disc_fill, disc_shadow,
    disc_shadow_hovered, hub_border, hub_fill, hub_shadow, hub_shadow_hovered, ink,
    inset_ring_border, label, label_hovered, muted, needle_highlight, platter_border, platter_fill,
    platter_shadow,
};

use super::motion::{
    Stage, blend_shadows, ease_out_cubic, ellipse_path, gather_shift, keyframes, press_at,
    slot_heading, smooth_towards, stage_at, wrap_to_pi,
};
use super::{RingLook, RingView};
use crate::ipc::OverlayCommand;

/// How fast a disc, its caption and the hub press in and ease back: the
/// prototype's 170 ms ease, which this reaches about 95% of in three time
/// constants.
const HOVER_TAU: f32 = 0.08;
/// How fast the hub opens around the hovered action's glyph and name.
const HUB_TAU: f32 = 0.10;
/// How fast the needle shows and hides: the prototype's 140 ms fade.
const AIM_TAU: f32 = 0.05;
/// A turn of the needle takes this long, and overshoots by this fraction of
/// the turn, at most this far, before settling from this point of the turn.
const TURN_SECS: f32 = 0.30;
const OVERSHOOT_FRACTION: f32 = 0.04;
const OVERSHOOT_MAX: f32 = 4.0 * PI / 180.0;
const OVERSHOOT_AT: f32 = 0.78;
/// A click draws the stroke under the caption for this long.
const MARK_SECS: f32 = 0.30;
/// The stroke: at each point of it, how wide it is of its full width and
/// its alpha. It grows in fast and shrinks away.
const MARK_KEYS: [(f32, f32, f32); 3] = [(0.0, 0.4, 0.0), (0.3, 1.0, 0.85), (1.0, 0.65, 0.0)];
/// Segments in each round end of the needle.
const CAP_STEPS: u8 = 8;
const _: () = assert!(PLATTER * 2.0 <= DESIGN_EDGE, "the platter fits the window");

/// Hover, needle, press and open/close clocks for one open ring.
pub(super) struct Motion {
    hover_t: [f32; 8],
    /// Whether the pointer is on the hub, and how far the hub has pressed in.
    hub_hovered: bool,
    hub_press_t: f32,
    /// Where the needle points, and the turn it is making to get there.
    aim: f32,
    turn: Option<Turn>,
    /// How far the needle is shown: a ring nobody has hovered yet points
    /// nowhere.
    aim_t: f32,
    /// 0 with the hub showing the cancel cross, 1 showing the hovered action.
    hub_t: f32,
    /// The disc a click chose, for its press and the stroke under it.
    pressed: Option<(ActionRingSlot, Instant)>,
    born: Instant,
    last_tick: Instant,
    close_started: Option<Instant>,
}

/// A turn of the needle from one heading to another.
#[derive(Clone, Copy)]
struct Turn {
    from: f32,
    to: f32,
    started: Instant,
}

impl Default for Motion {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            hover_t: [0.0; 8],
            hub_hovered: false,
            hub_press_t: 0.0,
            // The prototype's needle starts pointing right, unseen.
            aim: slot_heading(ActionRingSlot::Right),
            turn: None,
            aim_t: 0.0,
            hub_t: 0.0,
            pressed: None,
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

    /// Remember the disc a click chose, so its press and stroke play into the
    /// exit.
    fn commit(&mut self, slot: ActionRingSlot, now: Instant) {
        self.pressed = Some((slot, now));
    }

    fn advance(&mut self, hovered: Option<ActionRingSlot>, now: Instant, reduce: bool) {
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        let ease = |current: f32, target: f32, tau: f32| {
            if reduce {
                target
            } else {
                smooth_towards(current, target, dt, tau)
            }
        };
        for slot in ActionRingSlot::ALL {
            let target = if hovered == Some(slot) { 1.0 } else { 0.0 };
            let index = slot.index();
            self.hover_t[index] = ease(self.hover_t[index], target, HOVER_TAU);
        }
        let hub_target = if self.hub_hovered { 1.0 } else { 0.0 };
        self.hub_press_t = ease(self.hub_press_t, hub_target, HOVER_TAU);
        // A new heading starts a turn from wherever the needle shows this
        // frame, so a turn cut short carries on rather than jumping.
        if let Some(heading) = hovered.map(slot_heading)
            && wrap_to_pi(heading - self.aim).abs() > 0.001
        {
            let from = self.needle_angle(now, reduce);
            self.aim = heading;
            self.turn = (!reduce).then_some(Turn {
                from,
                to: heading,
                started: now,
            });
        }
        let aim_target = if hovered.is_some() { 1.0 } else { 0.0 };
        self.aim_t = ease(self.aim_t, aim_target, AIM_TAU);
        self.hub_t = ease(self.hub_t, aim_target, HUB_TAU);
    }

    /// Where the needle points this frame: its heading, or the turn towards
    /// it, which runs a little past the heading and settles back.
    fn needle_angle(&self, now: Instant, reduce: bool) -> f32 {
        let Some(Turn { from, to, started }) = self.turn.filter(|_| !reduce) else {
            return self.aim;
        };
        let t = now.saturating_duration_since(started).as_secs_f32() / TURN_SECS;
        if t >= 1.0 {
            return to;
        }
        let change = wrap_to_pi(to - from);
        let overshoot = (change * OVERSHOOT_FRACTION).clamp(-OVERSHOOT_MAX, OVERSHOOT_MAX);
        if t < OVERSHOOT_AT {
            from + (change + overshoot) * ease_out_cubic(t / OVERSHOOT_AT)
        } else {
            to + overshoot * (1.0 - ease_out_cubic((t - OVERSHOOT_AT) / (1.0 - OVERSHOOT_AT)))
        }
    }

    /// The pressed disc, while the click's press plays: its slot, how far it
    /// sinks and how much of its size it keeps.
    fn press(&self, now: Instant, reduce: bool) -> Option<(ActionRingSlot, f32, f32)> {
        let (slot, started) = self.pressed.filter(|_| !reduce)?;
        press_at(started, now).map(|(sink, kept)| (slot, sink, kept))
    }

    /// The stroke under the pressed caption, while it plays: its slot, how
    /// wide it is of its full width, and its alpha.
    fn mark(&self, now: Instant, reduce: bool) -> Option<(ActionRingSlot, f32, f32)> {
        let (slot, started) = self.pressed.filter(|_| !reduce)?;
        let t = now.saturating_duration_since(started).as_secs_f32() / MARK_SECS;
        (t < 1.0).then(|| {
            let (width, alpha) = keyframes(&MARK_KEYS, t, ease_out_cubic);
            (slot, width, alpha)
        })
    }

    /// How far open the ring is this frame.
    fn stage(&self, now: Instant, reduce: bool) -> Stage {
        stage_at(self.born, self.close_started, now, reduce)
    }
}

/// The panel's edge and how far the layout is scaled down to fit it. `fit`
/// is 1 on every display at least [`DESIGN_EDGE`] tall and wide.
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

    /// Top-left of `slot`'s node. Everything scales with the panel, so nodes
    /// that clear each other at the design size clear each other at any.
    fn slot_origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        let (w, h) = self.node_size();
        (
            self.edge / 2.0 + x * RADIUS * self.fit - w / 2.0,
            self.edge / 2.0 + y * RADIUS * self.fit - h / 2.0,
        )
    }

    /// How far a node's content sits back towards the centre when the node
    /// is `reach` of the way out.
    fn gather_shift(self, slot: ActionRingSlot, reach: f32) -> (f32, f32) {
        gather_shift(slot, RADIUS * self.fit, reach)
    }

    /// The slot's centre, for tests that aim at it.
    #[cfg(test)]
    fn slot_center(self, slot: ActionRingSlot) -> Point<Pixels> {
        let (x, y) = slot.unit_offset();
        point(
            px(self.edge / 2.0 + x * RADIUS * self.fit),
            px(self.edge / 2.0 + y * RADIUS * self.fit),
        )
    }

    fn node_size(self) -> (f32, f32) {
        (NODE_W * self.fit, NODE_H * self.fit)
    }
}

impl RingView {
    /// The panel, `edge` pixels square (see [`super::panel_edge`]). It takes
    /// the background click itself so the stock root handler never closes
    /// without the exit.
    pub(super) fn porcelain_panel(
        &mut self,
        edge: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let now = Instant::now();
        let reduce = cx.reduce_motion();
        let layout = Layout::fitting(edge);
        let RingLook::Porcelain(motion) = &mut self.look else {
            unreachable!("porcelain_panel is only drawn for a Porcelain ring");
        };
        motion.advance(self.hovered, now, reduce);
        let stage = motion.stage(now, reduce);
        let hover_t = motion.hover_t;
        let needle = (motion.needle_angle(now, reduce), motion.aim_t);
        let press = motion.press(now, reduce);
        let mark = motion.mark(now, reduce);
        let hub_t = motion.hub_t;
        let hub_press_t = motion.hub_press_t;
        let hovered = self
            .hovered
            .and_then(|slot| self.invocation.slots.get(&slot));
        let hovered_icon = hovered.map(|presentation| presentation.icon.asset_path());
        let hovered_caption = hovered.map(super::slot_caption);
        let nodes = ActionRingSlot::ALL
            .into_iter()
            .zip(hover_t)
            .zip(stage.nodes)
            .filter_map(|((slot, hover_t), stage)| {
                let playing_on =
                    |(at, a, b): (ActionRingSlot, f32, f32)| (at == slot).then_some((a, b));
                let node = Node {
                    slot,
                    hover_t,
                    press: press.and_then(playing_on),
                    mark: mark.and_then(playing_on),
                    stage,
                };
                node_element(self, node, layout, cx)
            })
            .collect::<Vec<_>>();
        let hub = Hub {
            hovered_icon,
            caption: hovered_caption,
            open: hub_t,
            press_t: hub_press_t,
            dial: stage.dial,
        };

        div()
            .id("ring-porcelain")
            .relative()
            .size(px(layout.edge))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                let session_id = this.invocation.session_id;
                this.close(OverlayCommand::Cancel { session_id }, window, cx);
            }))
            .child(platter(layout, stage.dial))
            .child(inset_ring(layout, stage.dial))
            .child(needle_canvas(layout, stage.dial, needle, reduce))
            .children(nodes)
            .child(hub_element(hub, layout, cx))
    }
}

/// A circle of `diameter` centred in the panel.
fn centred(layout: Layout, diameter: f32) -> gpui::Div {
    div()
        .absolute()
        .left(px(layout.edge / 2.0 - diameter / 2.0))
        .top(px(layout.edge / 2.0 - diameter / 2.0))
        .size(px(diameter))
        .rounded_full()
}

/// The glazed platter that holds the whole ring, grown in with it.
fn platter(layout: Layout, (scale, opacity): (f32, f32)) -> impl IntoElement {
    centred(layout, 2.0 * PLATTER * layout.fit * scale)
        .bg(platter_fill())
        .border_1()
        .border_color(platter_border())
        .shadow(platter_shadow())
        .opacity(opacity)
}

/// The thin ring inside the platter that the needle runs along.
fn inset_ring(layout: Layout, (scale, opacity): (f32, f32)) -> impl IntoElement {
    centred(layout, 2.0 * INSET_RING * layout.fit * scale)
        .border_1()
        .border_color(inset_ring_border())
        .opacity(opacity)
}

/// The needle: a terracotta capsule just inside the inset ring, pointing at
/// `angle` and shown `aim_t` of the way, with a lit tip.
fn needle_canvas(
    layout: Layout,
    (scale, opacity): (f32, f32),
    (angle, aim_t): (f32, f32),
    reduce: bool,
) -> impl IntoElement {
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            if !reduce {
                window.request_animation_frame();
            }
            let shown = aim_t * opacity;
            if shown < 0.01 {
                return;
            }
            let center = bounds.center();
            let scale = scale * layout.fit;
            let along = |radius: f32| {
                point(
                    center.x + px(radius * scale * angle.cos()),
                    center.y + px(radius * scale * angle.sin()),
                )
            };
            let width = NEEDLE_W * scale;
            if let Some(path) = capsule_path(along(NEEDLE_INNER), along(NEEDLE_OUTER), width) {
                window.paint_path(path, accent().alpha(shown));
            }
            if let Some(tip) = ellipse_path(along(NEEDLE_OUTER), width * 0.3, None) {
                window.paint_path(tip, needle_highlight().alpha(0.9 * shown));
            }
        },
    )
    .absolute()
    .size_full()
}

/// A stroke of `width` from `from` to `to` with round ends, as one filled
/// path: a stroked line with discs on its ends would paint their overlap
/// twice and show through a translucent colour.
fn capsule_path(from: Point<Pixels>, to: Point<Pixels>, width: f32) -> Option<gpui::Path<Pixels>> {
    let radius = width / 2.0;
    if radius <= 0.25 {
        return None;
    }
    let heading = f32::from(to.y - from.y).atan2(f32::from(to.x - from.x));
    let around = |center: Point<Pixels>, start: f32, step: u8| {
        let angle = start + f32::from(step) / f32::from(CAP_STEPS) * PI;
        point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        )
    };
    let mut builder = PathBuilder::fill();
    // Round the far end from one side to the other, then round the near end
    // back.
    builder.move_to(around(to, heading - PI / 2.0, 0));
    for step in 1..=CAP_STEPS {
        builder.line_to(around(to, heading - PI / 2.0, step));
    }
    for step in 0..=CAP_STEPS {
        builder.line_to(around(from, heading + PI / 2.0, step));
    }
    builder.close();
    builder.build().ok()
}

/// What one node is drawn from this frame.
#[derive(Clone, Copy)]
struct Node {
    slot: ActionRingSlot,
    hover_t: f32,
    /// The click's press, while it plays on this node: how far the disc
    /// sinks and how much of its size it keeps.
    press: Option<(f32, f32)>,
    /// The stroke, while it plays under this node: its width of the full one
    /// and its alpha.
    mark: Option<(f32, f32)>,
    /// How far out from the centre the node is, its scale and its opacity.
    stage: (f32, f32, f32),
}

/// One node: the hit box that stays put, and inside it the disc and caption
/// that gather out from the centre as the ring opens, shade in under the
/// pointer without moving, the disc pressed in by a click with the stroke it
/// draws under the caption.
fn node_element(
    view: &RingView,
    node: Node,
    layout: Layout,
    cx: &mut Context<RingView>,
) -> Option<gpui::AnyElement> {
    let Node {
        slot,
        hover_t,
        press,
        mark,
        stage: (reach, scale, opacity),
    } = node;
    let presentation = view.invocation.slots.get(&slot)?;
    let icon_path = presentation.icon.asset_path();
    let caption = super::slot_caption(presentation);
    let (left, top) = layout.slot_origin(slot);
    let (shift_x, shift_y) = layout.gather_shift(slot, reach);
    let (w, h) = layout.node_size();
    let fit = layout.fit;
    let session_id = view.invocation.session_id;
    // The pointer moves nothing: the hover is colour and shade alone. A
    // click presses the disc in and back.
    let (sink, kept) = press.unwrap_or((0.0, 1.0));
    let disc = DISC * fit * kept * scale;
    let disc_top = NODE_PAD_TOP * fit + (DISC * fit - disc) / 2.0 + sink * fit;
    let caption_top = (NODE_PAD_TOP + DISC + NODE_GAP) * fit;
    Some(
        div()
            .id(("ring-slot", slot.index()))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(w))
            .h(px(h))
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .left(px(shift_x))
                    .top(px(shift_y))
                    .size_full()
                    .opacity(opacity)
                    .child(disc_element(
                        icon_path,
                        hover_t,
                        ((w - disc) / 2.0, disc_top),
                        (disc, DISC_GLYPH * fit * scale),
                    ))
                    .child(
                        div()
                            .absolute()
                            .left(px(fit))
                            .top(px(caption_top))
                            .w(px((NODE_W - 2.0) * fit))
                            .text_size(px(LABEL * fit))
                            .text_center()
                            .text_color(lerp_hsla(label(), label_hovered(), hover_t))
                            .truncate()
                            .child(caption),
                    )
                    .when_some(mark, |content, (width, alpha)| {
                        let width = MARK_W * fit * width;
                        content.child(
                            div()
                                .absolute()
                                .left(px((w - width) / 2.0))
                                .bottom(px(fit))
                                .w(px(width))
                                .h(px(MARK_H * fit))
                                .rounded(px(fit))
                                .bg(accent())
                                .opacity(alpha),
                        )
                    }),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                this.hover(slot, *hovered, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if let RingLook::Porcelain(motion) = &mut this.look {
                    motion.commit(slot, Instant::now());
                }
                this.close(OverlayCommand::Activate { session_id, slot }, window, cx);
            }))
            .into_any_element(),
    )
}

/// A disc face at `(left, top)` of its node, `size` across with a glyph of
/// `glyph`: white ceramic that sinks under a crescent of shade as `hover_t`
/// presses it in.
fn disc_element(
    icon_path: &'static str,
    hover_t: f32,
    (left, top): (f32, f32),
    (size, glyph): (f32, f32),
) -> impl IntoElement {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .size(px(size))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(disc_fill())
        .border_1()
        .border_color(lerp_hsla(disc_border(), disc_border_hovered(), hover_t))
        .shadow(blend_shadows(disc_shadow(), disc_shadow_hovered(), hover_t))
        .child(
            // An svg paints in its own colour only, never an inherited one.
            gpui::svg()
                .path(icon_path)
                .size(px(glyph))
                .flex_none()
                .text_color(ink()),
        )
}

/// What the hub is drawn from this frame.
struct Hub {
    hovered_icon: Option<&'static str>,
    caption: Option<gpui::SharedString>,
    /// 0 showing the cancel cross, 1 showing the hovered action.
    open: f32,
    /// How far the hub has pressed in under the pointer.
    press_t: f32,
    /// The platter's scale and opacity, which the hub shares.
    dial: (f32, f32),
}

/// The hub: the cancel cross at rest, opening into the hovered action's
/// glyph and name as `open` rises, and shading in under the pointer with
/// the cross turning terracotta, since pressing it closes the ring.
fn hub_element(hub: Hub, layout: Layout, cx: &mut Context<RingView>) -> impl IntoElement {
    let Hub {
        hovered_icon,
        caption,
        open,
        press_t,
        dial: (scale, opacity),
    } = hub;
    let fit = layout.fit;
    let open = ease_out_cubic(open);
    let glyph = HUB_GLYPH * fit * scale;
    let size = HUB * fit * scale;
    centred(layout, size)
        .id("ring-cancel")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(HUB_GAP * fit))
        .bg(hub_fill())
        .border_1()
        .border_color(lerp_hsla(hub_border(), disc_border_hovered(), press_t))
        .shadow(blend_shadows(hub_shadow(), hub_shadow_hovered(), press_t))
        .opacity(opacity)
        .cursor_pointer()
        .child(
            div()
                .relative()
                .size(px(glyph))
                .flex_none()
                .child(
                    gpui::svg()
                        .path(RING_CANCEL_ICON)
                        .absolute()
                        .inset_0()
                        .size(px(glyph))
                        .text_color(lerp_hsla(muted(), close_ink(), press_t))
                        .opacity(1.0 - open),
                )
                .when_some(hovered_icon, |slot, icon| {
                    slot.child(
                        gpui::svg()
                            .path(icon)
                            .absolute()
                            .inset_0()
                            .size(px(glyph))
                            .text_color(accent())
                            .opacity(open),
                    )
                }),
        )
        .when_some(caption, |hub, caption| {
            hub.child(
                div()
                    .w(px((HUB - 16.0) * fit))
                    .text_size(px(HUB_LABEL * fit))
                    .font_weight(FontWeight::MEDIUM)
                    .text_center()
                    .text_color(ink())
                    .truncate()
                    .opacity(open)
                    .child(caption),
            )
        })
        .on_hover(cx.listener(|this, hovered, _, cx| {
            if let RingLook::Porcelain(motion) = &mut this.look {
                motion.hub_hovered = *hovered;
            }
            cx.notify();
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            let session_id = this.invocation.session_id;
            this.close(OverlayCommand::Cancel { session_id }, window, cx);
        }))
}

/// Centre of `slot`'s node within a full-size panel, for tests that aim the
/// pointer at it.
#[cfg(test)]
pub(super) fn slot_center(slot: ActionRingSlot) -> (f32, f32) {
    let at = Layout::full().slot_center(slot);
    (f32::from(at.x), f32::from(at.y))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn nodes_sit_at_the_prototype_radius_inside_the_platter() {
        let (x, y) = slot_center(ActionRingSlot::Right);
        assert!((x - (DESIGN_EDGE / 2.0 + RADIUS)).abs() < 0.01);
        assert!((y - DESIGN_EDGE / 2.0).abs() < 0.01);
        let (left, top) = Layout::full().slot_origin(ActionRingSlot::Top);
        assert!((left + NODE_W / 2.0 - DESIGN_EDGE / 2.0).abs() < 0.01);
        assert!((top + NODE_H / 2.0 - (DESIGN_EDGE / 2.0 - RADIUS)).abs() < 0.01);
        // Every corner of every node is on the platter.
        for slot in ActionRingSlot::ALL {
            let (left, top) = Layout::full().slot_origin(slot);
            for (x, y) in [
                (left, top),
                (left + NODE_W, top),
                (left, top + NODE_H),
                (left + NODE_W, top + NODE_H),
            ] {
                let from_centre =
                    ((x - DESIGN_EDGE / 2.0).powi(2) + (y - DESIGN_EDGE / 2.0).powi(2)).sqrt();
                assert!(from_centre <= PLATTER, "{slot:?} hangs off the platter");
            }
        }
    }

    #[test]
    fn neighbouring_nodes_never_overlap_however_short_the_panel() {
        for edge in [400.0, 360.0, 300.0, 240.0] {
            let layout = Layout::fitting(px(edge));
            let rect = |slot: ActionRingSlot| {
                let (left, top) = layout.slot_origin(slot);
                let (w, h) = layout.node_size();
                (left, top, left + w, top + h)
            };
            for (index, slot) in ActionRingSlot::ALL.into_iter().enumerate() {
                let next = ActionRingSlot::ALL[(index + 1) % ActionRingSlot::ALL.len()];
                let (a, b) = (rect(slot), rect(next));
                let apart = a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1;
                assert!(apart, "{slot:?} and {next:?} overlap on a {edge} px panel");
                assert!(
                    a.0 >= 0.0 && a.1 >= 0.0 && a.2 <= layout.edge && a.3 <= layout.edge,
                    "{slot:?} leaves the {edge} px panel"
                );
            }
        }
    }

    #[test]
    fn the_needle_turns_the_short_way_and_overshoots_a_little() {
        let mut motion = Motion::default();
        let start = motion.born;
        let ms = |ms: u64| start + Duration::from_millis(ms);
        // From the right, where it starts, up to the top: a quarter turn.
        motion.advance(Some(ActionRingSlot::Top), start, false);
        let top = slot_heading(ActionRingSlot::Top);
        let early = motion.needle_angle(ms(60), false);
        assert!(early < 0.0 && early > top, "mid-turn the needle is between");
        let past = motion.needle_angle(ms(234), false) - top;
        assert!(
            past < 0.0 && past.abs() <= OVERSHOOT_MAX + 0.001,
            "at {OVERSHOOT_AT} of the turn it has run {past} past the top"
        );
        assert!((motion.needle_angle(ms(300), false) - top).abs() < 0.001);
        assert!((motion.needle_angle(ms(900), false) - top).abs() < 0.001);
        // A half turn overshoots as far as it is allowed to, no further.
        motion.advance(Some(ActionRingSlot::Bottom), ms(1000), false);
        let bottom = slot_heading(ActionRingSlot::Bottom);
        let past = (motion.needle_angle(ms(1234), false) - bottom).abs();
        assert!((past - OVERSHOOT_MAX).abs() < 0.001, "capped at {past}");
        // Reduced motion snaps.
        let mut still = Motion::default();
        still.advance(Some(ActionRingSlot::Left), start, true);
        let left = slot_heading(ActionRingSlot::Left);
        assert!((still.needle_angle(ms(1), true) - left).abs() < f32::EPSILON);
    }

    #[test]
    fn the_needle_fades_when_nothing_is_hovered_and_holds_its_heading() {
        let mut motion = Motion::default();
        let mut now = motion.born;
        assert!(motion.aim_t.abs() < 0.001, "a fresh ring points nowhere");
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(Some(ActionRingSlot::Left), now, false);
        }
        assert!(motion.aim_t > 0.9);
        let left = slot_heading(ActionRingSlot::Left);
        assert!(wrap_to_pi(motion.needle_angle(now, false) - left).abs() < 0.001);
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(None, now, false);
        }
        assert!(motion.aim_t < 0.1, "the needle fades with nothing hovered");
        assert!(
            wrap_to_pi(motion.needle_angle(now, false) - left).abs() < 0.001,
            "but the heading holds"
        );
    }

    #[test]
    fn the_press_and_stroke_play_once_then_stop() {
        let mut motion = Motion::default();
        let now = motion.born;
        let ms = |ms: u64| now + Duration::from_millis(ms);
        assert!(motion.press(now, false).is_none());
        assert!(motion.mark(now, false).is_none());
        motion.commit(ActionRingSlot::Bottom, now);
        let (slot, sink, kept) = motion.press(ms(80), false).expect("the disc is pressed");
        assert_eq!(slot, ActionRingSlot::Bottom);
        assert!(
            sink > 2.0 && kept < 0.96,
            "a third of the way in it is in and a touch smaller"
        );
        let (_, sink, kept) = motion.press(ms(180), false).expect("the disc eases back");
        assert!(
            sink > 0.0 && sink < 1.0 && kept > 0.98 && kept < 1.0,
            "three quarters in it is most of the way back out"
        );
        let (_, width, alpha) = motion.mark(ms(90), false).expect("the stroke shows");
        assert!(width > 0.99 && alpha > 0.84);
        assert!(motion.press(ms(250), false).is_none());
        assert!(motion.mark(ms(310), false).is_none());
        assert!(
            motion.press(ms(80), true).is_none(),
            "reduced motion presses nothing"
        );
        assert!(motion.mark(ms(90), true).is_none());
    }

    #[test]
    fn the_hub_presses_in_under_the_pointer_and_eases_back() {
        let mut motion = Motion::default();
        let mut now = motion.born;
        motion.hub_hovered = true;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(None, now, false);
        }
        assert!(motion.hub_press_t > 0.9);
        motion.hub_hovered = false;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(None, now, false);
        }
        assert!(motion.hub_press_t < 0.1);
    }

    #[test]
    fn the_ring_gathers_its_nodes_out_on_opening_and_back_on_closing() {
        let mut motion = Motion::default();
        let born = motion.born;
        let ms = |ms: u64| born + Duration::from_millis(ms);
        let opening = motion.stage(born, false);
        assert!(
            opening.dial.1 < 0.01 && opening.dial.0 < 0.2,
            "the platter starts small and unseen"
        );
        for (reach, _, opacity) in opening.nodes {
            assert!(
                reach.abs() < 0.001 && opacity < 0.001,
                "the nodes start at the centre"
            );
        }
        let layout = Layout::full();
        let (dx, dy) = layout.gather_shift(ActionRingSlot::Right, 0.0);
        assert!(
            (dx + RADIUS).abs() < 0.001 && dy.abs() < 0.001,
            "gathered to the centre"
        );
        let (dx, dy) = layout.gather_shift(ActionRingSlot::Right, 1.0);
        assert!(dx.abs() < 0.001 && dy.abs() < 0.001, "out in place");
        let half_way = motion.stage(ms(150), false);
        let (first, last) = (half_way.nodes[0].0, half_way.nodes[7].0);
        assert!(first > last, "the first node is further out than the last");
        assert!(first > 0.3 && first < 1.04);
        let open = motion.stage(ms(500), false);
        assert!((open.dial.0 - 1.0).abs() < 0.001 && (open.dial.1 - 1.0).abs() < 0.001);
        for (reach, scale, opacity) in open.nodes {
            assert!((reach - 1.0).abs() < 0.001 && (scale - 1.0).abs() < 0.001);
            assert!((opacity - 1.0).abs() < 0.001);
        }
        assert!(motion.begin_close(ms(1000)));
        let closing = motion.stage(ms(1100), false);
        assert!(
            closing.dial.0 < 1.0 && closing.nodes[0].0 < 1.0,
            "closing gathers back"
        );
        let closed = motion.stage(ms(1300), false);
        assert!(closed.dial.1 < 0.001);
        for (reach, _, opacity) in closed.nodes {
            assert!(reach.abs() < 0.001 && opacity < 0.001);
        }
        // Reduced motion is open at once, and gone at once.
        let mut still = Motion::default();
        let still_open = still.stage(still.born, true);
        assert!((still_open.dial.1 - 1.0).abs() < f32::EPSILON);
        assert!(
            still_open
                .nodes
                .iter()
                .all(|&(reach, _, _)| (reach - 1.0).abs() < f32::EPSILON)
        );
        still.begin_close(still.born);
        assert!(still.stage(still.born, true).dial.1 < f32::EPSILON);
    }
}

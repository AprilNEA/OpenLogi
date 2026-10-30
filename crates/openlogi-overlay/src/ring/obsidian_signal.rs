//! The `Obsidian Signal` theme: Obsidian's dark cards, orbit lines, ticks,
//! sector and signal hub on one opaque graphite chassis that holds the whole
//! ring. A card presses in under the pointer and rounds into a circle with
//! its glyph and caption still inside; a click presses it further and sends
//! Obsidian's wave out from it and flash through the hub; the hub presses in
//! under the pointer and closes the ring. The ring opens by gathering its
//! cards out from the centre and closes by gathering them back.
//!
//! The look at rest is [`openlogi_ui::ring_style::obsidian_signal`], shared
//! with the settings app's preview. The dial, the aim and the click are
//! [`super::obsidian`]'s, drawn at this theme's size; the card and its hover
//! are this theme's own.

use std::time::Instant;

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement as _, Styled, div, px,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::ring_style::lerp_hsla;
pub(super) use openlogi_ui::ring_style::obsidian_signal::DESIGN_EDGE;
use openlogi_ui::ring_style::obsidian_signal::{
    AMBIENT, CARD_GAP, CARD_GLYPH, CARD_H, CARD_LABEL, CARD_RADIUS, CARD_W, CELL, CELL_GAP,
    CELL_LABEL, CHASSIS, RADIUS, SHRINK, ambient_fill, ambient_shadow, card_border,
    cell_border_hovered, cell_fill, cell_shadow, cell_shadow_hovered, chassis_border, chassis_fill,
    chassis_shadow, cross, glyph, hub_border, hub_shadow, hub_shadow_hovered, signal, text,
};

use super::motion::{Stage, blend_shadows, gather_shift, press_at, smooth_towards, stage_at};
use super::obsidian::{Aim, Click, Hub, HubLook, centred_in, dial_canvas, hub_element};
use super::{RingLook, RingView};
use crate::ipc::OverlayCommand;

/// How fast a card rounds into its circle and presses in, and eases back,
/// and the hub with it: the prototype's 240 ms ease, which this reaches
/// about 95% of in three time constants.
const HOVER_TAU: f32 = 0.09;
/// How fast the hub opens around the hovered action's glyph and name.
const HUB_TAU: f32 = 0.10;
const _: () = assert!(CHASSIS * 2.0 <= DESIGN_EDGE, "the chassis fits the window");

/// Hover, aim, click and open/close clocks for one open ring.
pub(super) struct Motion {
    hover_t: [f32; 8],
    /// Whether the pointer is on the hub, and how far the hub has pressed in.
    hub_hovered: bool,
    hub_press_t: f32,
    aim: Aim,
    /// 0 with the hub showing the cancel cross, 1 showing the hovered action.
    hub_t: f32,
    click: Click,
    born: Instant,
    last_tick: Instant,
    close_started: Option<Instant>,
}

impl Default for Motion {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            hover_t: [0.0; 8],
            hub_hovered: false,
            hub_press_t: 0.0,
            aim: Aim::default(),
            hub_t: 0.0,
            click: Click::default(),
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
        self.aim.advance(hovered, dt, reduce);
        let open = if hovered.is_some() { 1.0 } else { 0.0 };
        self.hub_t = ease(self.hub_t, open, HUB_TAU);
    }

    /// The pressed card, while the click's press plays: its slot, how far it
    /// sinks and how much of its size it keeps.
    fn press(&self, now: Instant, reduce: bool) -> Option<(ActionRingSlot, f32, f32)> {
        let (slot, started) = self.click.started().filter(|_| !reduce)?;
        press_at(started, now).map(|(sink, kept)| (slot, sink, kept))
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

    /// Top-left of `slot`'s hit box. Everything scales with the panel, so
    /// cards that clear each other at the design size clear each other at
    /// any.
    fn slot_origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        let (w, h) = self.card_size();
        (
            self.edge / 2.0 + x * RADIUS * self.fit - w / 2.0,
            self.edge / 2.0 + y * RADIUS * self.fit - h / 2.0,
        )
    }

    /// How far a card sits back towards the centre when it is `reach` of
    /// the way out.
    fn gather_shift(self, slot: ActionRingSlot, reach: f32) -> (f32, f32) {
        gather_shift(slot, RADIUS * self.fit, reach)
    }

    /// The slot's centre, for tests that aim at it.
    #[cfg(test)]
    fn slot_center(self, slot: ActionRingSlot) -> gpui::Point<Pixels> {
        let (x, y) = slot.unit_offset();
        gpui::point(
            px(self.edge / 2.0 + x * RADIUS * self.fit),
            px(self.edge / 2.0 + y * RADIUS * self.fit),
        )
    }

    fn card_size(self) -> (f32, f32) {
        (CARD_W * self.fit, CARD_H * self.fit)
    }
}

/// A card's shape between its resting box and its circle: its width and
/// height, corner radius, the gap between glyph and caption, and the
/// caption's width.
struct CellShape {
    w: f32,
    h: f32,
    radius: f32,
    gap: f32,
    label_w: f32,
}

impl CellShape {
    /// The shape `rounded` of the way from the box to the circle, at `fit`.
    fn at(rounded: f32, fit: f32) -> Self {
        let between = |rest: f32, circle: f32| (rest + (circle - rest) * rounded) * fit;
        Self {
            w: between(CARD_W, CELL),
            h: between(CARD_H, CELL),
            radius: between(CARD_RADIUS, CELL / 2.0),
            gap: between(CARD_GAP, CELL_GAP),
            label_w: between(CARD_W - 6.0, CELL_LABEL),
        }
    }
}

impl RingView {
    /// The panel, `edge` pixels square (see [`super::panel_edge`]). It takes
    /// the background click itself so the stock root handler never closes
    /// without the exit.
    pub(super) fn obsidian_signal_panel(
        &mut self,
        edge: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let now = Instant::now();
        let reduce = cx.reduce_motion();
        let layout = Layout::fitting(edge);
        let RingLook::ObsidianSignal(motion) = &mut self.look else {
            unreachable!("obsidian_signal_panel is only drawn for an Obsidian Signal ring");
        };
        motion.advance(self.hovered, now, reduce);
        let stage = motion.stage(now, reduce);
        let hover_t = motion.hover_t;
        let aim = motion.aim;
        let wave = motion.click.wave(now, reduce);
        let flash = motion.click.flash(now, reduce);
        let chosen = motion.click.slot();
        let press = motion.press(now, reduce);
        let (hub_t, hub_press_t) = (motion.hub_t, motion.hub_press_t);
        let hovered = self
            .hovered
            .and_then(|slot| self.invocation.slots.get(&slot));
        let hovered_icon = hovered.map(|presentation| presentation.icon.asset_path());
        let hovered_caption = hovered.map(super::slot_caption);
        let cards = ActionRingSlot::ALL
            .into_iter()
            .zip(hover_t)
            .zip(stage.nodes)
            .filter_map(|((slot, hover_t), stage)| {
                let card = Card {
                    slot,
                    hover_t,
                    pressed: chosen == Some(slot),
                    press: press.and_then(|(at, sink, kept)| (at == slot).then_some((sink, kept))),
                    stage,
                };
                card_node(self, card, layout, cx)
            })
            .collect::<Vec<_>>();
        let (grown, opacity) = stage.dial;
        // Obsidian's dial and hub, at this theme's size.
        let dial_fit = layout.fit * SHRINK * grown;
        let hub = Hub {
            hovered_icon,
            caption: hovered_caption,
            open: hub_t,
            flash,
            opacity,
            look: HubLook {
                border: lerp_hsla(hub_border(), cell_border_hovered(), hub_press_t),
                shadow: blend_shadows(hub_shadow(), hub_shadow_hovered(), hub_press_t),
                cross: lerp_hsla(cross(), signal(), hub_press_t),
            },
        };

        div()
            .id("ring-obsidian-signal")
            .relative()
            .size(px(layout.edge))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                let session_id = this.invocation.session_id;
                this.close(OverlayCommand::Cancel { session_id }, window, cx);
            }))
            .child(chassis(layout, stage.dial))
            .child(ambient(layout, stage.dial))
            .child(dial_canvas(dial_fit, opacity, aim, wave, reduce))
            .children(cards)
            .child(
                hub_element(hub, layout.edge, dial_fit, cx).on_hover(cx.listener(
                    |this, hovered, _, cx| {
                        if let RingLook::ObsidianSignal(motion) = &mut this.look {
                            motion.hub_hovered = *hovered;
                        }
                        cx.notify();
                    },
                )),
            )
    }
}

/// The graphite chassis that holds the whole ring, grown in with it.
fn chassis(layout: Layout, (scale, opacity): (f32, f32)) -> impl IntoElement {
    centred_in(layout.edge, 2.0 * CHASSIS * layout.fit * scale)
        .bg(chassis_fill())
        .border_1()
        .border_color(chassis_border())
        .shadow(chassis_shadow())
        .opacity(opacity)
}

/// The soft disc under the dial, grown in with it.
fn ambient(layout: Layout, (scale, opacity): (f32, f32)) -> impl IntoElement {
    centred_in(layout.edge, 2.0 * AMBIENT * layout.fit * scale)
        .bg(ambient_fill())
        .shadow(ambient_shadow())
        .opacity(opacity)
}

/// What one card is drawn from this frame.
#[derive(Clone, Copy)]
struct Card {
    slot: ActionRingSlot,
    hover_t: f32,
    /// Whether a click chose this card.
    pressed: bool,
    /// The click's press, while it plays on this card: how far the card
    /// sinks and how much of its size it keeps.
    press: Option<(f32, f32)>,
    /// How far out from the centre the card is, its scale and its opacity.
    stage: (f32, f32, f32),
}

/// One card: the hit box that stays put, and inside it the card that gathers
/// out from the centre as the ring opens, presses in and rounds into a
/// circle under the pointer, and sinks under a click.
fn card_node(
    view: &RingView,
    card: Card,
    layout: Layout,
    cx: &mut Context<RingView>,
) -> Option<gpui::AnyElement> {
    let Card {
        slot,
        hover_t,
        pressed,
        press,
        stage: (reach, scale, opacity),
    } = card;
    let presentation = view.invocation.slots.get(&slot)?;
    let icon_path = presentation.icon.asset_path();
    let caption = super::slot_caption(presentation);
    let (left, top) = layout.slot_origin(slot);
    let (shift_x, shift_y) = layout.gather_shift(slot, reach);
    let (w, h) = layout.card_size();
    let session_id = view.invocation.session_id;
    let (sink, kept) = press.unwrap_or((0.0, 1.0));
    // The card keeps its centre as it changes shape, and as it shrinks under
    // the gather and the press.
    let fit = layout.fit * kept * scale;
    let shape = CellShape::at(hover_t, fit);
    let glyph_ink = if pressed {
        signal()
    } else {
        lerp_hsla(glyph(), text(), hover_t)
    };
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
                    .left(px(shift_x + (w - shape.w) / 2.0))
                    .top(px(shift_y + (h - shape.h) / 2.0 + sink * layout.fit))
                    .w(px(shape.w))
                    .h(px(shape.h))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(shape.gap))
                    .rounded(px(shape.radius))
                    .bg(cell_fill(pressed))
                    .border_1()
                    .border_color(lerp_hsla(
                        card_border(pressed),
                        cell_border_hovered(),
                        hover_t,
                    ))
                    .shadow(blend_shadows(cell_shadow(), cell_shadow_hovered(), hover_t))
                    .opacity(opacity)
                    .child(
                        gpui::svg()
                            .path(icon_path)
                            .size(px(CARD_GLYPH * fit))
                            .flex_none()
                            .text_color(glyph_ink),
                    )
                    .child(
                        div()
                            .w(px(shape.label_w))
                            .text_size(px(CARD_LABEL * layout.fit))
                            .text_center()
                            .text_color(text())
                            .truncate()
                            .child(caption),
                    ),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                this.hover(slot, *hovered, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if let RingLook::ObsidianSignal(motion) = &mut this.look {
                    motion.click.commit(slot, Instant::now());
                }
                this.close(OverlayCommand::Activate { session_id, slot }, window, cx);
            }))
            .into_any_element(),
    )
}

/// Centre of `slot`'s card within a full-size panel, for tests that aim the
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
    fn cards_sit_at_obsidians_radius_on_the_chassis() {
        let (x, y) = slot_center(ActionRingSlot::Right);
        assert!((x - (DESIGN_EDGE / 2.0 + RADIUS)).abs() < 0.01);
        assert!((y - DESIGN_EDGE / 2.0).abs() < 0.01);
        // Every corner of every hit box, and the circle a hovered card rounds
        // into, is on the chassis.
        for slot in ActionRingSlot::ALL {
            let (left, top) = Layout::full().slot_origin(slot);
            let (cx, cy) = (left + CARD_W / 2.0, top + CARD_H / 2.0);
            let from_centre = |x: f32, y: f32| {
                ((x - DESIGN_EDGE / 2.0).powi(2) + (y - DESIGN_EDGE / 2.0).powi(2)).sqrt()
            };
            for (x, y) in [
                (left, top),
                (left + CARD_W, top),
                (left, top + CARD_H),
                (left + CARD_W, top + CARD_H),
            ] {
                assert!(
                    from_centre(x, y) <= CHASSIS,
                    "{slot:?} hangs off the chassis"
                );
            }
            assert!(
                from_centre(cx, cy) + CELL / 2.0 <= CHASSIS,
                "{slot:?}'s circle hangs off the chassis"
            );
        }
    }

    #[test]
    fn neighbouring_cards_never_overlap_however_short_the_panel() {
        for edge in [400.0, 360.0, 300.0, 240.0] {
            let layout = Layout::fitting(px(edge));
            let rect = |slot: ActionRingSlot| {
                let (left, top) = layout.slot_origin(slot);
                let (w, h) = layout.card_size();
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
    fn a_card_rounds_into_a_circle_as_the_pointer_arrives() {
        let rest = CellShape::at(0.0, 1.0);
        assert_eq!(
            (rest.w, rest.h, rest.radius),
            (CARD_W, CARD_H, CARD_RADIUS),
            "at rest it is the card"
        );
        let circle = CellShape::at(1.0, 1.0);
        assert_eq!(
            (circle.w, circle.h, circle.radius),
            (CELL, CELL, CELL / 2.0),
            "under the pointer it is the circle"
        );
        assert!(circle.label_w < rest.label_w, "the caption fits the circle");
        let half = CellShape::at(0.5, 1.0);
        assert!(half.w < rest.w && half.w > circle.w);
        assert!(half.h > rest.h && half.h < circle.h);
        let shrunk = CellShape::at(1.0, 0.5);
        assert!(
            (shrunk.w - CELL / 2.0).abs() < 0.001,
            "fit scales the shape"
        );
    }

    #[test]
    fn the_card_and_the_hub_press_in_under_the_pointer_and_ease_back() {
        let mut motion = Motion::default();
        let mut now = motion.born;
        motion.hub_hovered = true;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(Some(ActionRingSlot::Left), now, false);
        }
        let left = ActionRingSlot::Left.index();
        assert!(motion.hover_t[left] > 0.9 && motion.hub_press_t > 0.9);
        assert!(motion.aim.t > 0.9, "the sector turns to the hovered card");
        motion.hub_hovered = false;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            motion.advance(None, now, false);
        }
        assert!(motion.hover_t[left] < 0.1 && motion.hub_press_t < 0.1);
        // A click presses the card in, then it eases back; reduced motion
        // presses nothing.
        motion.click.commit(ActionRingSlot::Left, now);
        let (slot, sink, kept) = motion
            .press(now + Duration::from_millis(80), false)
            .expect("the card is pressed");
        assert_eq!(slot, ActionRingSlot::Left);
        assert!(sink > 2.0 && kept < 0.96);
        assert!(
            motion
                .press(now + Duration::from_millis(250), false)
                .is_none()
        );
        assert!(
            motion
                .press(now + Duration::from_millis(80), true)
                .is_none()
        );
    }
}

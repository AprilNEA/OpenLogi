//! The `Obsidian` theme: dark glass cards on a dial of orbit lines and ticks,
//! around a hub that names the hovered action. A mint sector and dot turn to
//! face the hovered card, a lit halo rises under it with the light following
//! the pointer, and a click sends a wave out from the card and a flash through
//! the hub.
//!
//! The look at rest is [`openlogi_ui::ring_style::obsidian`], shared with the
//! settings app's preview; everything that moves is here. The dial, the aim
//! and click clocks and the hub are drawn by [`super::obsidian_signal`] too,
//! at its own size.

use std::f32::consts::PI;
use std::time::Instant;

use gpui::{
    BoxShadow, Context, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, Point, StatefulInteractiveElement as _, Styled, Window, canvas, div,
    linear_color_stop, linear_gradient, point, prelude::FluentBuilder as _, px,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::ring_style::lerp_hsla;
pub(super) use openlogi_ui::ring_style::obsidian::DESIGN_EDGE;
use openlogi_ui::ring_style::obsidian::{
    AMBIENT, CARD_GAP, CARD_GLYPH, CARD_H, CARD_LABEL, CARD_RADIUS, CARD_W, HUB, HUB_GLYPH,
    HUB_LABEL, HUB_OPEN, ORBIT_INNER, ORBIT_OUTER, RADIUS, ambient_fill, ambient_shadow,
    card_border, card_border_hovered, card_fill, card_shadow, card_shadow_hovered, glyph,
    halo_border, halo_shadow, hub_border, hub_fill, hub_shadow, orbit_inner, orbit_outer, signal,
    text,
};

use super::motion::{
    Stage, blend_shadows, ease_out_cubic, ellipse_path, gather_shift, hex, lerp_angle,
    paint_gradient_arc, slot_heading, smooth_towards, spring_towards, stage_at, sweep_arc,
};
use super::{RingLook, RingView};
use crate::ipc::OverlayCommand;

/// How fast a card lifts and its halo rises or fades: the prototype's 230 ms
/// ease, which this reaches about 95% of in three time constants.
const HOVER_TAU: f32 = 0.11;
/// How fast the hub opens around the hovered action's glyph and name.
const HUB_TAU: f32 = 0.10;
/// How closely the halo's light follows the pointer; it only hides the frame
/// quantisation, the prototype moves it at once.
const LIGHT_TAU: f32 = 0.05;
/// How fast the sector and dot turn to a newly hovered card.
const AIM_TAU: f32 = 0.08;
/// The halo's rim arrives with a turn of this much, taking this long.
const RIM_ARRIVE_RAD: f32 = -48.0 * PI / 180.0;
const RIM_ARRIVE_SECS: f32 = 0.44;
/// The wave a click sends out from its card, and the flash through the hub.
const WAVE_SECS: f32 = 0.42;
const FLASH_SECS: f32 = 0.29;
/// Where the light sits on a halo nobody has pointed into yet.
const DEFAULT_LIGHT: (f32, f32) = (0.34, 0.24);
/// How far a hovered card rises.
const LIFT: f32 = 2.0;
const HALO: f32 = 116.0;
/// A halo grows from this much of its size as it appears, on a spring that
/// runs a touch past full size before it settles, and sinks back towards it
/// as it fades: about 2 px of overshoot, at rest in a third of a second.
const BUBBLE_HIDDEN: f32 = 0.84;
const BUBBLE_HZ: f32 = 3.2;
const BUBBLE_DAMPING: f32 = 0.6;
/// How fast a halo fades in, and out; leaving is the quicker, so moving
/// between cards never shows two full halos.
const BUBBLE_IN_TAU: f32 = 0.06;
const BUBBLE_OUT_TAU: f32 = 0.045;
const WAVE: f32 = 105.0;
/// The sector: a wedge this wide around the aimed card, on an annulus between
/// these radii, in six steps of alpha peaking just before its middle.
const SECTOR_SPAN: f32 = 45.0 * PI / 180.0;
const SECTOR_INNER: f32 = 141.8;
const SECTOR_OUTER: f32 = 160.1;
const SECTOR_ALPHA: [f32; 6] = [0.08, 0.20, 0.25, 0.18, 0.10, 0.04];
const _: () = assert!(SECTOR_ALPHA.len() == 6, "the sector is cut into six steps");
/// The tick band and the dot's orbit, just outside the outer orbit line.
const TICK_INNER: f32 = 169.1;
const TICK_OUTER: f32 = 173.6;
const TICKS: u8 = 48;
const DOT_ORBIT: f32 = 161.2;
const _: () = assert!(
    HUB_OPEN / 2.0 < ORBIT_INNER,
    "the open hub stays inside the inner orbit"
);

/// Hover, halo, aim, light, wave and open/close clocks for one open ring.
pub(super) struct Motion {
    hover_t: [f32; 8],
    bubbles: [Bubble; 8],
    /// Where the light sits in the hovered card's box, 0..1 on each axis.
    light: (f32, f32),
    aim: Aim,
    /// 0 with the hub showing the cancel cross, 1 showing the hovered action.
    hub_t: f32,
    /// When the current hover began, for the rim's arrival turn.
    rim_born: Option<(ActionRingSlot, Instant)>,
    click: Click,
    born: Instant,
    last_tick: Instant,
    close_started: Option<Instant>,
}

/// One card's halo: its size as a fraction of the full one, how fast that is
/// changing, and its opacity. A halo that reverses half way carries on from
/// where it is, never jumping.
#[derive(Clone, Copy)]
struct Bubble {
    scale: f32,
    velocity: f32,
    opacity: f32,
}

impl Default for Bubble {
    fn default() -> Self {
        Self {
            scale: BUBBLE_HIDDEN,
            velocity: 0.0,
            opacity: 0.0,
        }
    }
}

impl Bubble {
    fn advance(&mut self, shown: bool, dt: f32, reduce: bool) {
        let (scale, opacity, tau) = if shown {
            (1.0, 1.0, BUBBLE_IN_TAU)
        } else {
            (BUBBLE_HIDDEN, 0.0, BUBBLE_OUT_TAU)
        };
        if reduce {
            *self = Self {
                scale,
                velocity: 0.0,
                opacity,
            };
            return;
        }
        (self.scale, self.velocity) = spring_towards(
            (self.scale, self.velocity),
            scale,
            dt,
            BUBBLE_HZ,
            BUBBLE_DAMPING,
        );
        self.opacity = smooth_towards(self.opacity, opacity, dt, tau);
    }
}

/// The sector's and dot's heading, and how far they are shown at all: a ring
/// nobody has hovered yet does not point anywhere. The heading turns the
/// short way to a newly hovered card and holds when the pointer leaves.
#[derive(Clone, Copy)]
pub(super) struct Aim {
    pub(super) angle: f32,
    pub(super) t: f32,
}

impl Default for Aim {
    fn default() -> Self {
        Self {
            angle: slot_heading(ActionRingSlot::Top),
            t: 0.0,
        }
    }
}

impl Aim {
    pub(super) fn advance(&mut self, hovered: Option<ActionRingSlot>, dt: f32, reduce: bool) {
        let heading = hovered.map(slot_heading);
        self.angle = if reduce {
            heading.unwrap_or(self.angle)
        } else {
            heading.map_or(self.angle, |heading| {
                lerp_angle(self.angle, heading, dt, AIM_TAU)
            })
        };
        let target = if hovered.is_some() { 1.0 } else { 0.0 };
        self.t = if reduce {
            target
        } else {
            smooth_towards(self.t, target, dt, AIM_TAU)
        };
    }
}

/// The card a click chose, for the wave it sends out and the flash through
/// the hub, which play into the exit.
#[derive(Clone, Copy, Default)]
pub(super) struct Click(Option<(ActionRingSlot, Instant)>);

impl Click {
    pub(super) fn commit(&mut self, slot: ActionRingSlot, now: Instant) {
        self.0 = Some((slot, now));
    }

    /// The chosen card, if any.
    pub(super) fn slot(self) -> Option<ActionRingSlot> {
        self.0.map(|(slot, _)| slot)
    }

    /// The chosen card and when the click landed, for whatever else plays
    /// from it.
    pub(super) fn started(self) -> Option<(ActionRingSlot, Instant)> {
        self.0
    }

    /// The click wave, while it plays: its card, its scale and its alpha.
    pub(super) fn wave(self, now: Instant, reduce: bool) -> Option<(ActionRingSlot, f32, f32)> {
        if reduce {
            return None;
        }
        let (slot, started) = self.0?;
        let t = now.saturating_duration_since(started).as_secs_f32() / WAVE_SECS;
        (t < 1.0).then(|| (slot, 0.7 + 0.72 * ease_out_cubic(t), 0.65 * (1.0 - t)))
    }

    /// How bright the hub's flash is, 0 when none plays.
    pub(super) fn flash(self, now: Instant, reduce: bool) -> f32 {
        if reduce {
            return 0.0;
        }
        self.0.map_or(0.0, |(_, started)| {
            let t = now.saturating_duration_since(started).as_secs_f32() / FLASH_SECS;
            let pulse = if t < 0.22 {
                t / 0.22
            } else {
                1.0 - ease_out_cubic((t - 0.22) / 0.78)
            };
            0.14 * pulse.clamp(0.0, 1.0)
        })
    }
}

impl Default for Motion {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            hover_t: [0.0; 8],
            bubbles: [Bubble::default(); 8],
            light: DEFAULT_LIGHT,
            aim: Aim::default(),
            hub_t: 0.0,
            rim_born: None,
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

    /// Remember the card a click chose, so its wave and the hub's flash play
    /// into the exit.
    fn commit(&mut self, slot: ActionRingSlot, now: Instant) {
        self.click.commit(slot, now);
    }

    fn advance(
        &mut self,
        hovered: Option<ActionRingSlot>,
        pointer_in_card: Option<(f32, f32)>,
        now: Instant,
        reduce: bool,
    ) {
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
            self.bubbles[index].advance(hovered == Some(slot), dt, reduce);
        }
        self.light = match (reduce, pointer_in_card) {
            (true, _) => DEFAULT_LIGHT,
            (false, Some((x, y))) => (
                smooth_towards(self.light.0, x, dt, LIGHT_TAU),
                smooth_towards(self.light.1, y, dt, LIGHT_TAU),
            ),
            (false, None) => self.light,
        };
        // The rim turns into place on every arrival, so leaving a card forgets
        // it: coming back to the same card is an arrival again.
        match hovered {
            Some(slot) if self.rim_born.is_none_or(|(born, _)| born != slot) => {
                self.rim_born = Some((slot, now));
            }
            Some(_) => {}
            None => self.rim_born = None,
        }
        self.aim.advance(hovered, dt, reduce);
        let open = if hovered.is_some() { 1.0 } else { 0.0 };
        self.hub_t = if reduce {
            open
        } else {
            smooth_towards(self.hub_t, open, dt, HUB_TAU)
        };
    }

    /// How far open the ring is this frame: the dial grows in and the cards
    /// gather out from the centre, and back on closing, as Porcelain's do.
    fn stage(&self, now: Instant, reduce: bool) -> Stage {
        stage_at(self.born, self.close_started, now, reduce)
    }

    /// Advance the clocks to `now`, with the pointer at `pointer` within a
    /// panel `edge` pixels square, and copy out what this frame draws.
    pub(super) fn frame(
        &mut self,
        hovered: Option<ActionRingSlot>,
        pointer: Point<Pixels>,
        edge: Pixels,
        now: Instant,
        reduce: bool,
    ) -> Frame {
        let layout = Layout::fitting(edge);
        // The light follows the pointer within the hovered card's box. Render
        // runs every frame while motion is on, so the pointer is this
        // frame's; no move handler is needed.
        let pointer_in_card = hovered.map(|slot| {
            let (left, top) = layout.slot_origin(slot);
            let (w, h) = layout.card_size();
            (
                ((f32::from(pointer.x) - left) / w).clamp(0.18, 0.82),
                ((f32::from(pointer.y) - top) / h).clamp(0.16, 0.84),
            )
        });
        self.advance(hovered, pointer_in_card, now, reduce);
        Frame {
            hover_t: self.hover_t,
            bubbles: self.bubbles,
            light: self.light,
            rim: self.rim_angle(now, reduce),
            wave: self.wave(now, reduce),
            flash: self.flash(now, reduce),
            committed: self.click.slot(),
            aim: self.aim,
            hub_t: self.hub_t,
            stage: self.stage(now, reduce),
            reduce,
        }
    }

    /// How far the halo's rim still has to turn to settle, in radians.
    fn rim_angle(&self, now: Instant, reduce: bool) -> f32 {
        if reduce {
            return 0.0;
        }
        self.rim_born.map_or(0.0, |(_, born)| {
            let t = now.saturating_duration_since(born).as_secs_f32() / RIM_ARRIVE_SECS;
            RIM_ARRIVE_RAD * (1.0 - ease_out_cubic(t))
        })
    }

    /// The click wave, while it plays: its card, its scale and its alpha.
    fn wave(&self, now: Instant, reduce: bool) -> Option<(ActionRingSlot, f32, f32)> {
        self.click.wave(now, reduce)
    }

    /// How bright the hub's flash is, 0 when none plays.
    fn flash(&self, now: Instant, reduce: bool) -> f32 {
        self.click.flash(now, reduce)
    }
}

/// What one frame of the panel is drawn from: the clocks as they stand,
/// copied out of the motion so the panel borrows nothing.
#[derive(Clone, Copy)]
pub(super) struct Frame {
    hover_t: [f32; 8],
    bubbles: [Bubble; 8],
    light: (f32, f32),
    /// How far the halo's rim still has to turn, in radians.
    rim: f32,
    wave: Option<(ActionRingSlot, f32, f32)>,
    flash: f32,
    /// The card a click chose.
    committed: Option<ActionRingSlot>,
    aim: Aim,
    hub_t: f32,
    stage: Stage,
    reduce: bool,
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

    /// Top-left of `slot`'s card. Everything scales with the panel, so cards
    /// that clear each other at the design size clear each other at any.
    fn slot_origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        let (w, h) = self.card_size();
        (
            self.edge / 2.0 + x * RADIUS * self.fit - w / 2.0,
            self.edge / 2.0 + y * RADIUS * self.fit - h / 2.0,
        )
    }

    fn slot_center(self, slot: ActionRingSlot) -> Point<Pixels> {
        let (x, y) = slot.unit_offset();
        point(
            px(self.edge / 2.0 + x * RADIUS * self.fit),
            px(self.edge / 2.0 + y * RADIUS * self.fit),
        )
    }

    fn card_size(self) -> (f32, f32) {
        (CARD_W * self.fit, CARD_H * self.fit)
    }

    /// How far a card's glass and halo sit back towards the centre when the
    /// card is `reach` of the way out.
    fn gather_shift(self, slot: ActionRingSlot, reach: f32) -> (f32, f32) {
        gather_shift(slot, RADIUS * self.fit, reach)
    }
}

impl RingView {
    /// The panel, `edge` pixels square (see [`super::panel_edge`]), drawn
    /// from `frame`. It takes the background click itself so the stock root
    /// handler never closes without the exit.
    pub(super) fn obsidian_panel(
        &self,
        frame: &Frame,
        edge: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            hover_t,
            bubbles,
            light,
            rim,
            wave,
            flash,
            committed,
            aim,
            hub_t,
            stage,
            reduce,
        } = *frame;
        let layout = Layout::fitting(edge);
        let hovered = self
            .hovered
            .and_then(|slot| self.invocation.slots.get(&slot));
        let hovered_icon = hovered.map(|presentation| presentation.icon.asset_path());
        let hovered_caption = hovered.map(super::slot_caption);
        let halos = ActionRingSlot::ALL
            .into_iter()
            .zip(bubbles)
            .zip(stage.nodes)
            .filter(|((_, bubble), (_, _, opacity))| bubble.opacity * opacity >= 0.01)
            .map(|((slot, bubble), node)| halo(slot, bubble, node, light, rim, layout))
            .collect::<Vec<_>>();
        let cards = ActionRingSlot::ALL
            .into_iter()
            .zip(hover_t)
            .zip(stage.nodes)
            .filter_map(|((slot, hover_t), stage)| {
                let card = Card {
                    slot,
                    hover_t,
                    light,
                    pressed: committed == Some(slot),
                    stage,
                    reduce,
                };
                card_node(self, card, layout, cx)
            })
            .collect::<Vec<_>>();
        let hub = Hub {
            hovered_icon,
            caption: hovered_caption,
            open: hub_t,
            flash,
            opacity: stage.dial.1,
            look: HubLook {
                border: lerp_hsla(hub_border(), signal().alpha(0.55), ease_out_cubic(hub_t)),
                shadow: hub_shadow(),
                cross: hex(0xa1ad_bdff),
            },
        };

        div()
            .id("ring-obsidian")
            .relative()
            .size(px(layout.edge))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                let session_id = this.invocation.session_id;
                this.close(OverlayCommand::Cancel { session_id }, window, cx);
            }))
            .child(ambient(layout, stage.dial))
            .child(dial_canvas(
                layout.fit * stage.dial.0,
                stage.dial.1,
                aim,
                wave,
                reduce,
            ))
            .children(halos)
            .children(cards)
            .child(hub_element(hub, layout.edge, layout.fit * stage.dial.0, cx))
    }
}

/// A circle of `diameter` centred in the panel.
fn centred(layout: Layout, diameter: f32) -> gpui::Div {
    centred_in(layout.edge, diameter)
}

/// A circle of `diameter` centred in a panel `edge` pixels square.
pub(super) fn centred_in(edge: f32, diameter: f32) -> gpui::Div {
    div()
        .absolute()
        .left(px(edge / 2.0 - diameter / 2.0))
        .top(px(edge / 2.0 - diameter / 2.0))
        .size(px(diameter))
        .rounded_full()
}

/// The soft disc under the dial, grown in with it.
fn ambient(layout: Layout, (scale, opacity): (f32, f32)) -> impl IntoElement {
    centred(layout, 2.0 * AMBIENT * layout.fit * scale)
        .bg(ambient_fill())
        .shadow(ambient_shadow())
        .opacity(opacity)
}

/// The dial: the two orbit lines, the tick band, the sector and the dot that
/// face the aimed card, and the click wave, filling the panel it is put in.
/// `scale` is the panel's fit times the dial's growth as the ring opens, and
/// `opacity` that growth's; the sizes are the design's times `scale`.
pub(super) fn dial_canvas(
    scale: f32,
    opacity: f32,
    aim: Aim,
    wave: Option<(ActionRingSlot, f32, f32)>,
    reduce: bool,
) -> impl IntoElement {
    let canvas = canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            if !reduce {
                window.request_animation_frame();
            }
            let center = bounds.center();
            paint_orbits(window, center, scale);
            paint_ticks(window, center, scale);
            if aim.t > 0.01 {
                paint_sector(window, center, scale, aim.angle, aim.t);
                paint_dot(window, center, scale, aim.angle, aim.t);
            }
            if let Some((slot, wave_scale, alpha)) = wave {
                let (x, y) = slot.unit_offset();
                let at = point(
                    center.x + px(x * RADIUS * scale),
                    center.y + px(y * RADIUS * scale),
                );
                // At full size the wave's last few pixels leave the window
                // on the four cardinal slots, at an alpha of about 0.03.
                if let Some(path) = ellipse_path(at, WAVE / 2.0 * wave_scale * scale, Some(1.0)) {
                    window.paint_path(path, signal().alpha(0.5 * alpha));
                }
            }
        },
    )
    .absolute()
    .size_full();
    // The paths take their opacity from the element they are painted in.
    div().absolute().size_full().opacity(opacity).child(canvas)
}

fn paint_orbits(window: &mut Window, center: Point<Pixels>, scale: f32) {
    for (radius, color) in [(ORBIT_OUTER, orbit_outer()), (ORBIT_INNER, orbit_inner())] {
        if let Some(path) = ellipse_path(center, radius * scale, Some(1.0)) {
            window.paint_path(path, color);
        }
    }
}

/// The 48 ticks as one stroked path: a path per tick would cost 48
/// tessellations a frame.
fn paint_ticks(window: &mut Window, center: Point<Pixels>, scale: f32) {
    let (inner, outer) = (TICK_INNER * scale, TICK_OUTER * scale);
    if inner <= 1.0 {
        return;
    }
    let mut builder = PathBuilder::stroke(px(1.0));
    for tick in 0..TICKS {
        let angle = f32::from(tick) / f32::from(TICKS) * 2.0 * PI;
        let (cos, sin) = (angle.cos(), angle.sin());
        builder.move_to(point(
            center.x + px(inner * cos),
            center.y + px(inner * sin),
        ));
        builder.line_to(point(
            center.x + px(outer * cos),
            center.y + px(outer * sin),
        ));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, hex(0x879b_ad45));
    }
}

/// The mint wedge on the annulus around the aimed card, brightest just before
/// its middle, as six filled steps.
fn paint_sector(window: &mut Window, center: Point<Pixels>, scale: f32, aim: f32, aim_t: f32) {
    let (inner, outer) = (SECTOR_INNER * scale, SECTOR_OUTER * scale);
    if inner <= 1.0 {
        return;
    }
    let step_span = SECTOR_SPAN / 6.0;
    let start = aim - SECTOR_SPAN / 2.0;
    let mut from = start;
    for alpha in SECTOR_ALPHA {
        let to = from + step_span;
        let mut builder = PathBuilder::fill();
        let arc = |radius: f32, angle: f32| {
            point(
                center.x + px(radius * angle.cos()),
                center.y + px(radius * angle.sin()),
            )
        };
        // Four straight pieces per edge are indistinguishable from the arc
        // at this size.
        let along = |radius: f32, k: f32| arc(radius, from + (to - from) * k / 4.0);
        builder.move_to(along(outer, 0.0));
        for k in [1.0, 2.0, 3.0, 4.0] {
            builder.line_to(along(outer, k));
        }
        for k in [4.0, 3.0, 2.0, 1.0, 0.0] {
            builder.line_to(along(inner, k));
        }
        builder.close();
        from = to;
        if let Ok(path) = builder.build() {
            window.paint_path(path, signal().alpha(alpha * aim_t));
        }
    }
}

/// The dot on the orbit at the aimed heading, with its glow.
fn paint_dot(window: &mut Window, center: Point<Pixels>, scale: f32, aim: f32, aim_t: f32) {
    let at = point(
        center.x + px(DOT_ORBIT * scale * aim.cos()),
        center.y + px(DOT_ORBIT * scale * aim.sin()),
    );
    for (radius, alpha) in [(9.0, 0.12), (5.0, 0.25), (2.5, 1.0)] {
        if let Some(path) = ellipse_path(at, radius * scale, None) {
            window.paint_path(path, signal().alpha(alpha * aim_t));
        }
    }
}

/// The lit halo under the hovered card: clear glass with the light where the
/// pointer is, a violet shade opposite, and a rim that turns into place. It
/// gathers out with its card as the ring opens.
fn halo(
    slot: ActionRingSlot,
    bubble: Bubble,
    (reach, scale, opacity): (f32, f32, f32),
    light: (f32, f32),
    rim: f32,
    layout: Layout,
) -> impl IntoElement {
    let size = HALO * layout.fit * bubble.scale * scale;
    let at = layout.slot_center(slot);
    let (shift_x, shift_y) = layout.gather_shift(slot, reach);
    let (dx, dy) = ((light.0 - 0.5) * size, (light.1 - 0.5) * size);
    // The glass is lit from where the pointer is: a sheen from the light's
    // side that clears to nothing past the middle, so the bubble stays clear
    // over a light background instead of clouding it.
    let lit_from = (-dx).atan2(dy).to_degrees();
    let glint = 20.0 * layout.fit;
    div()
        .absolute()
        .left(px(f32::from(at.x) + shift_x - size / 2.0))
        .top(px(f32::from(at.y) + shift_y - size / 2.0))
        .size(px(size))
        .rounded_full()
        .border_1()
        .border_color(halo_border())
        .bg(linear_gradient(
            lit_from,
            linear_color_stop(hex(0xe5ff_f538), 0.0),
            // The same colour, clear: a fade to transparent black would grey
            // the middle of the sweep.
            linear_color_stop(hex(0xe5ff_f500), 0.68),
        ))
        .shadow(halo_shadow())
        .opacity(bubble.opacity * opacity)
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(linear_gradient(
                    315.0,
                    linear_color_stop(hex(0xa797_e81f), 0.0),
                    linear_color_stop(hex(0xa797_e800), 0.55),
                )),
        )
        .child(
            div()
                .absolute()
                .left(px(size / 2.0 + dx - glint / 2.0))
                .top(px(size / 2.0 + dy - glint / 2.0))
                .size(px(glint))
                .rounded_full()
                .bg(hex(0xe5ff_f514))
                .shadow(vec![
                    gpui::BoxShadow::new(px(0.0), px(0.0), hex(0xe5ff_f51f)).blur_radius(px(10.0)),
                ]),
        )
        .child(halo_rim(size, rim))
}

/// The halo's rim: a mint arc and a violet arc that turn into place as the
/// halo arrives, and the fixed highlight along its upper left.
fn halo_rim(size: f32, rim: f32) -> impl IntoElement {
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = size / 2.0 - 0.5;
            let mint = hex(0x8ceb_d7ff);
            let pale = hex(0xe0ff_f4ff);
            paint_gradient_arc(
                window,
                center,
                radius,
                css(315.0) + rim,
                110.0 * PI / 180.0,
                1.0,
                move |falloff| lerp_hsla(pale, mint, falloff).alpha(0.6 * falloff),
            );
            let violet = hex(0xc8b6_ffff);
            paint_gradient_arc(
                window,
                center,
                radius,
                css(65.0) + rim,
                100.0 * PI / 180.0,
                1.0,
                move |falloff| violet.alpha(0.47 * falloff),
            );
            let highlight = radius - 7.0;
            for (from, to, color) in [
                (css(-81.0), css(9.0), hex(0xbaff_ef6b)),
                (css(-171.0), css(-81.0), hex(0xbaff_ef14)),
            ] {
                if let Some(path) = sweep_arc(center, highlight, from, to - from, 1.0) {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// A CSS angle (0 up, clockwise) in the canvas's radians (0 right, clockwise).
fn css(deg: f32) -> f32 {
    (deg - 90.0).to_radians()
}

/// What the hub is drawn from this frame.
pub(super) struct Hub {
    pub(super) hovered_icon: Option<&'static str>,
    pub(super) caption: Option<gpui::SharedString>,
    /// 0 showing the cancel cross, 1 showing the hovered action.
    pub(super) open: f32,
    /// The flash a click sends through it.
    pub(super) flash: f32,
    /// The dial's opacity, which the hub shares.
    pub(super) opacity: f32,
    pub(super) look: HubLook,
}

/// The hub's border, shadows and cross colour this frame: the theme's, eased
/// by whatever opens or presses it.
pub(super) struct HubLook {
    pub(super) border: Hsla,
    pub(super) shadow: Vec<BoxShadow>,
    pub(super) cross: Hsla,
}

/// The hub: the cancel cross at rest, opening into the hovered action's
/// glyph and name as `open` rises, with the flash a click sends through it,
/// centred in a panel `edge` pixels square at `fit` of its designed size.
/// It closes the ring on a click; a theme that presses it in under the
/// pointer adds its own hover.
///
/// The cross fades out as the action fades in, the glyph grows as it
/// arrives, and the glass brightens a little, so the hand-off reads as the
/// hub taking the action in rather than a swap.
pub(super) fn hub_element(
    hub: Hub,
    edge: f32,
    fit: f32,
    cx: &mut Context<RingView>,
) -> gpui::Stateful<gpui::Div> {
    let Hub {
        hovered_icon,
        caption,
        open,
        flash,
        opacity,
        look,
    } = hub;
    let open = ease_out_cubic(open);
    // The hub opens around the hovered action, wide enough to carry its
    // whole caption, wrapped, as In Motion's does: a card truncates a long
    // label, and user-authored labels can differ only at their end.
    let size = (HUB + (HUB_OPEN - HUB) * open) * fit;
    let glyph = (HUB_GLYPH + 6.0 * open) * fit;
    centred_in(edge, size)
        .id("ring-cancel")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(CARD_GAP * fit))
        .bg(hub_fill())
        .border_1()
        .border_color(look.border)
        .shadow(look.shadow)
        .text_color(signal())
        .opacity(opacity)
        .cursor_pointer()
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .bg(signal().alpha(0.08 * open)),
        )
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
                        .text_color(look.cross)
                        .opacity(1.0 - open),
                )
                .when_some(hovered_icon, |slot, icon| {
                    slot.child(
                        gpui::svg()
                            .path(icon)
                            .absolute()
                            .inset_0()
                            .size(px(glyph))
                            .text_color(signal())
                            .opacity(open),
                    )
                }),
        )
        .when_some(caption, |hub, caption| {
            hub.child(
                div()
                    .w(px((HUB_OPEN - 28.0) * fit))
                    .text_size(px(HUB_LABEL * fit))
                    .font_weight(FontWeight::MEDIUM)
                    .text_center()
                    .text_color(text())
                    .opacity(open)
                    .child(caption),
            )
        })
        .when(flash > 0.005, |hub| {
            hub.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .bg(hex(0xffff_ffff))
                    .opacity(flash),
            )
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            let session_id = this.invocation.session_id;
            this.close(OverlayCommand::Cancel { session_id }, window, cx);
        }))
}

/// What one card is drawn from this frame.
#[derive(Clone, Copy)]
struct Card {
    slot: ActionRingSlot,
    hover_t: f32,
    /// Where the pointer's light sits in the card's box, 0..1 on each axis.
    light: (f32, f32),
    pressed: bool,
    /// How far out from the centre the card is, its scale and its opacity.
    stage: (f32, f32, f32),
    reduce: bool,
}

/// How a card looks this frame: its lift, border, shadows, glyph colour and
/// where its sheen comes from, each eased by how far the pointer is onto it.
struct CardLook {
    rise: f32,
    border: Hsla,
    shadow: Vec<gpui::BoxShadow>,
    glyph_ink: Hsla,
    /// The angle the pointer's light sweeps the glass from, CSS degrees.
    lit_from: f32,
}

fn card_look(card: Card, layout: Layout) -> CardLook {
    let Card {
        hover_t,
        light,
        pressed,
        reduce,
        ..
    } = card;
    let (w, h) = layout.card_size();
    let rise = if reduce {
        0.0
    } else {
        LIFT * layout.fit * hover_t
    };
    let border = if pressed {
        card_border(true)
    } else {
        lerp_hsla(card_border(false), card_border_hovered(), hover_t)
    };
    // The drop shadow deepens with the lift instead of switching halfway.
    let shadow = if pressed {
        card_shadow(true)
    } else {
        blend_shadows(card_shadow(false), card_shadow_hovered(), hover_t)
    };
    let glyph_ink = if pressed {
        signal()
    } else {
        lerp_hsla(glyph(), text(), hover_t)
    };
    // The pointer's light sweeps across the glass: a soft sheen from the
    // light's side, with the mint tint of the halo under it.
    let (lx, ly) = ((light.0 - 0.5) * w, (light.1 - 0.5) * h);
    let lit_from = (-lx).atan2(ly).to_degrees();
    CardLook {
        rise,
        border,
        shadow,
        glyph_ink,
        lit_from,
    }
}

/// One card: the hit box that stays put, and inside it the glass card that
/// gathers out from the centre as the ring opens, lifts under the pointer and
/// catches its light.
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
        stage: (reach, scale, opacity),
        ..
    } = card;
    let presentation = view.invocation.slots.get(&slot)?;
    let icon_path = presentation.icon.asset_path();
    let caption = super::slot_caption(presentation);
    let (left, top) = layout.slot_origin(slot);
    let (w, h) = layout.card_size();
    let (shift_x, shift_y) = layout.gather_shift(slot, reach);
    // The glass shrinks about its own centre, inside the hit box.
    let fit = layout.fit * scale;
    let (glass_w, glass_h) = (w * scale, h * scale);
    let session_id = view.invocation.session_id;
    let CardLook {
        rise,
        border,
        shadow,
        glyph_ink,
        lit_from,
    } = card_look(card, layout);
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
                    .left(px(shift_x + (w - glass_w) / 2.0))
                    .top(px(shift_y + (h - glass_h) / 2.0 - rise))
                    .w(px(glass_w))
                    .h(px(glass_h))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(CARD_GAP * fit))
                    .rounded(px(CARD_RADIUS * fit))
                    .bg(card_fill(pressed))
                    .border_1()
                    .border_color(border)
                    .shadow(shadow)
                    .text_color(text())
                    .opacity(opacity)
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(CARD_RADIUS * fit))
                            .bg(linear_gradient(
                                lit_from,
                                linear_color_stop(hex(0xe5ff_f52a), 0.0),
                                linear_color_stop(hex(0x8ceb_d700), 0.62),
                            ))
                            .opacity(hover_t),
                    )
                    .child(
                        gpui::svg()
                            .path(icon_path)
                            .size(px(CARD_GLYPH * fit))
                            .flex_none()
                            .text_color(glyph_ink),
                    )
                    .child(
                        div()
                            .w(px((CARD_W - 6.0) * fit))
                            .text_size(px(CARD_LABEL * fit))
                            .text_center()
                            .truncate()
                            .child(caption),
                    )
                    .when(pressed, |card| card.child(pressed_dot(layout))),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                this.hover(slot, *hovered, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if let RingLook::Obsidian(motion) = &mut this.look {
                    motion.commit(slot, Instant::now());
                }
                this.close(OverlayCommand::Activate { session_id, slot }, window, cx);
            }))
            .into_any_element(),
    )
}

/// The mint dot in a pressed card's corner.
fn pressed_dot(layout: Layout) -> impl IntoElement {
    div()
        .absolute()
        .right(px(8.0 * layout.fit))
        .top(px(8.0 * layout.fit))
        .size(px(4.0 * layout.fit))
        .rounded_full()
        .bg(signal())
        .shadow(vec![
            gpui::BoxShadow::new(px(0.0), px(0.0), hex(0x8ceb_d788)).blur_radius(px(8.0)),
        ])
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
    use super::*;

    #[test]
    fn cards_sit_at_the_prototype_radius() {
        let (x, y) = slot_center(ActionRingSlot::Right);
        assert!((x - (DESIGN_EDGE / 2.0 + RADIUS)).abs() < 0.01);
        assert!((y - DESIGN_EDGE / 2.0).abs() < 0.01);
        let (left, top) = Layout::full().slot_origin(ActionRingSlot::Top);
        assert!((left + CARD_W / 2.0 - DESIGN_EDGE / 2.0).abs() < 0.01);
        assert!((top + CARD_H / 2.0 - (DESIGN_EDGE / 2.0 - RADIUS)).abs() < 0.01);
    }

    #[test]
    fn neighbouring_cards_never_overlap_however_short_the_panel() {
        for edge in [440.0, 360.0, 300.0, 240.0] {
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
    fn the_wave_and_flash_play_once_then_stop() {
        let mut motion = Motion::default();
        let now = motion.born;
        assert!(motion.wave(now, false).is_none());
        assert!(motion.flash(now, false) < 0.001);
        motion.commit(ActionRingSlot::Bottom, now);
        let (slot, scale, alpha) = motion
            .wave(now + std::time::Duration::from_millis(100), false)
            .expect("the wave plays after a click");
        assert_eq!(slot, ActionRingSlot::Bottom);
        assert!(scale > 0.7 && scale < 1.42);
        assert!(alpha > 0.0 && alpha < 0.65);
        assert!(motion.flash(now + std::time::Duration::from_millis(64), false) > 0.13);
        let later = now + std::time::Duration::from_millis(500);
        assert!(motion.wave(later, false).is_none());
        assert!(motion.flash(later, false) < 0.001);
        assert!(
            motion.wave(later, true).is_none(),
            "reduced motion plays no wave"
        );
    }

    #[test]
    fn the_halo_springs_in_fades_out_quicker_and_never_jumps() {
        let mut motion = Motion::default();
        let mut now = motion.born;
        let step = std::time::Duration::from_millis(16);
        let top = ActionRingSlot::Top.index();
        let mut peak: f32 = 0.0;
        let mut frames_to_show = None;
        for frame in 0..30 {
            now += step;
            let before = motion.bubbles[top].scale;
            motion.advance(Some(ActionRingSlot::Top), None, now, false);
            let bubble = motion.bubbles[top];
            assert!((bubble.scale - before).abs() < 0.04, "no jump in a frame");
            peak = peak.max(bubble.scale);
            if bubble.opacity > 0.9 && frames_to_show.is_none() {
                frames_to_show = Some(frame);
            }
        }
        assert!(peak > 1.005 && peak < 1.03, "a touch of overshoot: {peak}");
        assert!((motion.bubbles[top].scale - 1.0).abs() < 0.01);
        let mut frames_to_hide = None;
        for frame in 0..30 {
            now += step;
            let before = motion.bubbles[top].scale;
            motion.advance(None, None, now, false);
            assert!((motion.bubbles[top].scale - before).abs() < 0.04);
            if motion.bubbles[top].opacity < 0.1 && frames_to_hide.is_none() {
                frames_to_hide = Some(frame);
            }
        }
        let shown = frames_to_show.expect("the halo shows");
        let hidden = frames_to_hide.expect("the halo fades");
        assert!(
            hidden < shown,
            "fading out is the quicker: {hidden} vs {shown} frames"
        );
        let mut still = Motion::default();
        still.advance(Some(ActionRingSlot::Top), None, still.born, true);
        assert!((still.bubbles[top].scale - 1.0).abs() < f32::EPSILON);
        assert!((still.bubbles[top].opacity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_rim_arrives_on_every_visit_to_a_card() {
        let mut motion = Motion::default();
        let mut now = motion.born;
        let step = std::time::Duration::from_millis(16);
        motion.advance(Some(ActionRingSlot::Top), None, now, false);
        let first = motion.rim_born.expect("a hover starts the rim's arrival");
        now += step;
        motion.advance(None, None, now, false);
        assert!(
            motion.rim_born.is_none(),
            "leaving the card forgets the arrival"
        );
        now += step;
        motion.advance(Some(ActionRingSlot::Top), None, now, false);
        let again = motion.rim_born.expect("coming back is an arrival again");
        assert!(again.1 > first.1);
        assert!(
            motion.rim_angle(now, false) < -0.5,
            "the rim is mid-turn right after arriving"
        );
        assert!(
            motion.rim_angle(now, true).abs() < f32::EPSILON,
            "reduced motion skips the turn"
        );
    }

    #[test]
    fn the_aim_turns_the_short_way_and_fades_when_nothing_is_hovered() {
        let mut motion = Motion::default();
        let start = motion.born;
        assert!(motion.aim.t.abs() < 0.001, "a fresh ring points nowhere");
        let mut now = start;
        for _ in 0..30 {
            now += std::time::Duration::from_millis(16);
            motion.advance(Some(ActionRingSlot::Left), None, now, false);
        }
        assert!(motion.aim.t > 0.9);
        let left = slot_heading(ActionRingSlot::Left);
        assert!(super::super::motion::wrap_to_pi(motion.aim.angle - left).abs() < 0.1);
        for _ in 0..30 {
            now += std::time::Duration::from_millis(16);
            motion.advance(None, None, now, false);
        }
        assert!(motion.aim.t < 0.1, "the sector fades with nothing hovered");
        assert!(
            super::super::motion::wrap_to_pi(motion.aim.angle - left).abs() < 0.1,
            "but the heading holds"
        );
    }
}

//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Where a client can place its own windows, the window is the ring: a square
//! panel, sized by the ring's [`Geometry`], centred on the cursor and clamped
//! to its display. Wayland allows
//! neither half of that, so there each display gets a window covering it and
//! the same panel is drawn at the pointer, in the one window that heard it last.

use gpui::{
    Bounds, BoxShadow, ClickEvent, Context, Div, Entity, Hsla, InteractiveElement, IntoElement,
    MouseMoveEvent, ParentElement, PathBuilder, Pixels, Point, Render, SharedString, Size,
    StatefulInteractiveElement as _, Styled, Subscription, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowId, WindowKind, WindowOptions, canvas, div,
    linear_color_stop, linear_gradient, point, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSize, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color::{self, neutral};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{self, ClickAwaySession, ShowingRing};

/// The ring's dimensions at one [`ActionRingSize`], in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    /// The window the ring is drawn in. It is larger than the ring so a
    /// hovered slot's label fits beside it; everything outside the slots is
    /// transparent.
    pub(crate) window: f32,
    /// From the centre to each slot's centre.
    radius: f32,
    slot: f32,
    /// A hovered slot grows by about 15% around the same centre.
    selected_slot: f32,
    /// How far the hovered slot's glow fades out past its edge.
    halo: f32,
    glyph: f32,
    selected_glyph: f32,
    cancel: f32,
    cancel_glyph: f32,
    /// How far the pointer must move from the centre before it aims at a
    /// slot. Inside it nothing is chosen: a click there, or a held trigger
    /// released there, closes the ring.
    dead_zone: f32,
}

impl Geometry {
    /// Medium is the standard ring. Each step scales the ring by about a
    /// fifth, and the window grows with it so a label beside a side slot keeps
    /// the same room.
    pub(crate) const fn of(size: ActionRingSize) -> Self {
        match size {
            ActionRingSize::Small => Self {
                window: 330.0,
                radius: 62.0,
                slot: 36.0,
                selected_slot: 42.0,
                halo: 11.0,
                glyph: 16.0,
                selected_glyph: 18.0,
                cancel: 24.0,
                cancel_glyph: 12.0,
                dead_zone: 20.0,
            },
            ActionRingSize::Medium => Self {
                window: 360.0,
                radius: 76.0,
                slot: 44.0,
                selected_slot: 50.0,
                halo: 14.0,
                glyph: 20.0,
                selected_glyph: 22.0,
                cancel: 28.0,
                cancel_glyph: 14.0,
                dead_zone: 24.0,
            },
            ActionRingSize::Large => Self {
                window: 400.0,
                radius: 96.0,
                slot: 54.0,
                selected_slot: 62.0,
                halo: 17.0,
                glyph: 24.0,
                selected_glyph: 26.0,
                cancel: 32.0,
                cancel_glyph: 16.0,
                dead_zone: 28.0,
            },
        }
    }

    /// The ring's footprint, out to a hovered slot's edge plus a margin.
    ///
    /// A click between the slots belongs to the ring, but macOS hands a click
    /// on a nearly transparent pixel to the window below: at 1/255 alpha it
    /// reached the app under the ring, at 0.01 the ring held it (measured on
    /// macOS 27). A mid grey at that alpha shifts any desktop colour by under 1%.
    fn footprint(self) -> f32 {
        2.0 * (self.radius + self.selected_slot / 2.0 + LABEL_GAP)
    }
}

/// Space between a hovered slot and its label.
const LABEL_GAP: f32 = 6.0;
/// Space a label keeps from the window edge, where its shadow would be cut off.
const LABEL_EDGE: f32 = 4.0;
const LABEL_MAX_WIDTH: f32 = 160.0;
const LABEL_LINE_HEIGHT: f32 = 16.0;
const LABEL_PADDING_X: f32 = 8.0;
const LABEL_PADDING_Y: f32 = 4.0;
/// A label wraps once, then ends in an ellipsis; [`LABEL_MAX_HEIGHT`] fits both
/// lines and the tail.
const LABEL_LINES: usize = 2;
/// The tail a label's bubble points at its slot with, and how far in from the
/// bubble's corner it starts.
const LABEL_TAIL_WIDTH: f32 = 12.0;
const LABEL_TAIL_HEIGHT: f32 = 6.0;
const LABEL_TAIL_INSET: f32 = 8.0;
const LABEL_MAX_HEIGHT: f32 = LABEL_LINE_HEIGHT * 2.0 + LABEL_PADDING_Y * 2.0 + LABEL_TAIL_HEIGHT;
/// Identifies the ring's windows to the compositor, whatever kind they are.
pub(crate) const RING_APP_ID: &str = "openlogi-action-ring";

/// The ring's neutral surfaces in one system appearance.
///
/// The slots float over whatever is on the desktop with nothing behind them.
/// An idle slot is a translucent disc with no edge, so the desktop shows
/// through it; only the selected slot is solid. In light the disc is a shade
/// off white, so it still reads over a white page. The ring follows the system
/// appearance rather than the settings app's theme, which this process does
/// not load. Only the accent and the shape of a grey are shared
/// (`openlogi_ui::color`); which greys the ring picks is local by nature.
#[derive(Clone, Copy)]
struct RingColors {
    slot: Hsla,
    glyph: Hsla,
    cancel: Hsla,
    cancel_border: Hsla,
    cancel_hover: Hsla,
    cancel_glyph: Hsla,
}

impl RingColors {
    const LIGHT: Self = Self {
        slot: neutral(0.94, 0.78),
        glyph: neutral(0.20, 1.0),
        cancel: neutral(0.93, 0.94),
        cancel_border: neutral(0.0, 0.10),
        cancel_hover: neutral(0.86, 0.96),
        cancel_glyph: neutral(0.30, 1.0),
    };

    const DARK: Self = Self {
        slot: neutral(0.20, 0.70),
        glyph: neutral(0.92, 1.0),
        cancel: neutral(0.26, 0.92),
        cancel_border: neutral(1.0, 0.12),
        cancel_hover: neutral(0.33, 0.95),
        cancel_glyph: neutral(0.80, 1.0),
    };

    const fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::LIGHT,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::DARK,
        }
    }
}

/// The accent deepened for a selected slot: the brand lightness sits too close
/// to the white glyph it carries, so the fill drops to `0.48` and the ring
/// around it rises to `0.78`. Both keep the brand hue and saturation, and read
/// the same over light and dark desktops.
const SELECTED_FILL_L: f32 = 0.48;
const SELECTED_BORDER_L: f32 = 0.78;
const SELECTED_GLYPH: Hsla = neutral(1.0, 1.0);
/// The selected slot's glow at its edge, as a share of the fill.
const SELECTED_GLOW_ALPHA: f32 = 0.55;

/// A label is a white speech bubble in either appearance, lifted off a white
/// page by a soft shadow.
const LABEL_FILL: Hsla = neutral(1.0, 1.0);
const LABEL_TEXT: Hsla = neutral(0.10, 1.0);
const LABEL_SHADOW: Hsla = neutral(0.0, 0.22);

/// See [`Geometry::footprint`].
const FOOTPRINT_FILL: Hsla = neutral(0.5, 0.01);

/// The beam's accent opacity where it leaves the centre and where it meets
/// the slot: it gathers toward the slot it points at.
const BEAM_ALPHA_AT_CENTRE: f32 = 0.08;
const BEAM_ALPHA_AT_SLOT: f32 = 0.38;

/// The slot a pointer `(x, y)` off the ring's centre aims at, among the
/// `filled` ones: the slot whose sector holds it, once it is out of the
/// dead zone. The pointer need not be over the slot, so a hand that
/// overshoots still chooses it.
fn aimed_slot(
    filled: impl IntoIterator<Item = ActionRingSlot>,
    geometry: Geometry,
    x: f32,
    y: f32,
) -> Option<ActionRingSlot> {
    if x.hypot(y) < geometry.dead_zone {
        return None;
    }
    let aimed = ActionRingSlot::toward(x, y);
    filled
        .into_iter()
        .any(|slot| slot == aimed)
        .then_some(aimed)
}

/// Corners of the beam from the centre to a highlighted `slot`, in panel
/// coordinates: the centre, the two points where its sides touch the grown
/// slot, and the slot's own centre, which the slot covers.
fn beam(slot: ActionRingSlot, geometry: Geometry) -> [Point<Pixels>; 4] {
    let centre = geometry.window / 2.0;
    let (x, y) = slot.unit_offset();
    let ring = geometry.radius;
    let radius = geometry.selected_slot / 2.0;
    // The sides run from the centre along the tangents to the slot's circle.
    let spread = (radius / ring).asin();
    let reach = ring.mul_add(ring, -radius * radius).sqrt();
    let side = |angle: f32| {
        let (sin, cos) = angle.sin_cos();
        point(
            px(reach.mul_add(x.mul_add(cos, -y * sin), centre)),
            px(reach.mul_add(x.mul_add(sin, y * cos), centre)),
        )
    };
    [
        point(px(centre), px(centre)),
        side(spread),
        point(px(x.mul_add(ring, centre)), px(y.mul_add(ring, centre))),
        side(-spread),
    ]
}

/// The beam drawn under a highlighted `slot`, over the whole panel.
fn beam_element(slot: ActionRingSlot, geometry: Geometry) -> impl IntoElement {
    let (x, y) = slot.unit_offset();
    // A gradient's angle starts at the top and turns clockwise.
    let toward_slot = x.atan2(-y).to_degrees().rem_euclid(360.0);
    let accent = color::accent_at_lightness(SELECTED_FILL_L);
    let fill = linear_gradient(
        toward_slot,
        linear_color_stop(accent.alpha(BEAM_ALPHA_AT_CENTRE), 0.0),
        linear_color_stop(accent.alpha(BEAM_ALPHA_AT_SLOT), 1.0),
    );
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let [apex, side, tip, other_side] =
                beam(slot, geometry).map(|corner| bounds.origin + corner);
            let mut path = PathBuilder::fill();
            path.move_to(apex);
            path.line_to(side);
            path.line_to(tip);
            path.line_to(other_side);
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, fill);
            }
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .size(px(geometry.window))
}

/// Which way a label grows from the point it is pinned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grow {
    Before,
    Centred,
    After,
}

impl Grow {
    /// Away from the ring's centre along one axis of a slot's unit offset.
    fn away_from_centre(offset: f32) -> Self {
        if offset > 0.01 {
            Self::After
        } else if offset < -0.01 {
            Self::Before
        } else {
            Self::Centred
        }
    }
}

/// The box a hovered slot's label may fill, in panel coordinates, and the
/// edges of it the label keeps to.
///
/// The label sits just outside its slot and grows away from the ring's centre,
/// so it never covers another slot, and it stays in the window. Straight out
/// from a side slot there is room only for a short word, so a side slot's label
/// sits on its upper shoulder instead.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LabelPlace {
    left: f32,
    top: f32,
    width: f32,
    horizontal: Grow,
    vertical: Grow,
}

impl LabelPlace {
    fn of(slot: ActionRingSlot, geometry: Geometry) -> Self {
        let centre = geometry.window / 2.0;
        let (x, y) = slot.unit_offset();
        let horizontal = Grow::away_from_centre(x);
        let ((anchor_x, anchor_y), vertical) = if Grow::away_from_centre(y) == Grow::Centred {
            (
                (
                    centre + x * (geometry.radius + LABEL_GAP),
                    centre - geometry.selected_slot / 2.0 - LABEL_GAP,
                ),
                Grow::Before,
            )
        } else {
            let reach = geometry.radius + geometry.selected_slot / 2.0 + LABEL_GAP;
            (
                (centre + x * reach, centre + y * reach),
                Grow::away_from_centre(y),
            )
        };
        let room_before = anchor_x - LABEL_EDGE;
        let room_after = geometry.window - LABEL_EDGE - anchor_x;
        let width = match horizontal {
            Grow::Before => room_before,
            Grow::After => room_after,
            Grow::Centred => 2.0 * room_before.min(room_after),
        }
        .min(LABEL_MAX_WIDTH);
        let left = match horizontal {
            Grow::Before => anchor_x - width,
            Grow::Centred => anchor_x - width / 2.0,
            Grow::After => anchor_x,
        };
        let top = match vertical {
            Grow::Before => anchor_y - LABEL_MAX_HEIGHT,
            Grow::Centred => anchor_y - LABEL_MAX_HEIGHT / 2.0,
            Grow::After => anchor_y,
        };
        Self {
            left,
            top,
            width,
            horizontal,
            vertical,
        }
    }
}

/// Where the ring's panel sits inside the window it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// The window was opened at the cursor, so the panel fills it.
    Window,
    /// The window covers a display, as each of the ring's windows does. The
    /// panel is drawn where the ring's [`RingPointer`] says, if it names this window.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "only Wayland covers a display")
    )]
    AtPointer,
}

/// Which of a ring's windows draws its panel, and where.
///
/// Wayland gives a ring one window per display, and the compositor decides
/// where each of them goes: GNOME stacks them all on one display (#1901). Each
/// window that heard the pointer would then draw a panel of its own, so the
/// ring's windows share this one record and only the last to hear the pointer
/// draws. That is the window on top, the only one a click can reach.
#[derive(Debug, Default)]
pub(crate) struct RingPointer {
    heard: Option<(WindowId, Point<Pixels>)>,
}

impl RingPointer {
    /// Record that `window` heard the pointer at `position`, reporting whether
    /// the panel moved.
    ///
    /// A window's first report places the panel: it is the move synthesized
    /// from pointer-enter, which says where the ring was opened. Later reports
    /// while that window keeps the panel do not move it.
    pub(crate) fn hear(&mut self, window: WindowId, position: Point<Pixels>) -> bool {
        if self.heard.is_some_and(|(drawing, _)| drawing == window) {
            return false;
        }
        self.heard = Some((window, position));
        true
    }

    /// Where `window` centres the panel: nowhere unless it heard the pointer last.
    pub(crate) fn center_in(&self, window: WindowId) -> Option<Point<Pixels>> {
        self.heard
            .filter(|&(drawing, _)| drawing == window)
            .map(|(_, center)| center)
    }
}

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    placement: Placement,
    /// Shared by every window of this ring.
    pointer: Entity<RingPointer>,
    /// Redraws this window when the panel moves, whichever window took it.
    _pointer_changes: Subscription,
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
}

impl RingView {
    /// Open a view on `invocation` in `window`, reporting interactions through
    /// `commands`. `pointer` is shared by all of the ring's windows.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
        placement: Placement,
        pointer: Entity<RingPointer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        let pointer_changes = cx.observe_in(&pointer, window, |this, pointer, window, cx| {
            // A window that stopped drawing the panel has no slot under the pointer.
            if pointer
                .read(cx)
                .center_in(window.window_handle().window_id())
                .is_none()
            {
                let _ = this.hover(None);
            }
            cx.notify();
        });
        Self {
            invocation,
            commands,
            hovered: None,
            placement,
            pointer,
            _pointer_changes: pointer_changes,
            _showing: showing,
        }
    }

    /// The ring session this view is showing.
    pub(crate) const fn session_id(&self) -> u64 {
        self.invocation.session_id
    }

    /// Report this ring cancelled. The window is closed by the caller, which
    /// is the only one holding the handle.
    pub(crate) fn cancel(&self) {
        let _ = self.commands.send(OverlayCommand::Cancel {
            session_id: self.invocation.session_id,
        });
    }

    /// Highlight `slot`, or none, and tell the agent when that changes: a
    /// held trigger's release chooses what the agent last heard. Answers
    /// whether it changed.
    fn hover(&mut self, slot: Option<ActionRingSlot>) -> bool {
        if self.hovered == slot {
            return false;
        }
        self.hovered = slot;
        let _ = self.commands.send(OverlayCommand::Hover {
            session_id: self.invocation.session_id,
            slot,
        });
        true
    }

    /// The dimensions this ring is drawn at.
    const fn geometry(&self) -> Geometry {
        Geometry::of(self.invocation.size)
    }

    fn slot_element(&self, slot: ActionRingSlot, colors: RingColors) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let geometry = self.geometry();
        let (size, glyph_size, glyph) = if selected {
            (
                geometry.selected_slot,
                geometry.selected_glyph,
                SELECTED_GLYPH,
            )
        } else {
            (geometry.slot, geometry.glyph, colors.glyph)
        };
        // A hovered slot grows around the centre it rests on.
        let (left, top) = slot.placement(geometry.window, geometry.radius, size);
        Some(
            div()
                .absolute()
                .left(px(left))
                .top(px(top))
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .map(|slot| {
                    if selected {
                        // The chosen slot glows in its own colour, fading out
                        // into the desktop; the others stay crisp.
                        let fill = color::accent_at_lightness(SELECTED_FILL_L);
                        slot.border_2()
                            .border_color(color::accent_at_lightness(SELECTED_BORDER_L))
                            .bg(fill)
                            .shadow(vec![BoxShadow {
                                color: fill.alpha(SELECTED_GLOW_ALPHA),
                                offset: point(px(0.0), px(0.0)),
                                blur_radius: px(geometry.halo),
                                spread_radius: px(geometry.halo / 4.0),
                                inset: false,
                            }])
                    } else {
                        slot.bg(colors.slot)
                    }
                })
                .child(svg().path(icon_path).size(px(glyph_size)).text_color(glyph))
                .into_any_element(),
        )
    }
}

impl RingView {
    /// The ring's square panel. Both placements anchor this same element.
    fn panel(&self, colors: RingColors) -> Div {
        let geometry = self.geometry();
        let window = geometry.window;
        let footprint = geometry.footprint();
        let hovered_label = self.hovered.and_then(|slot| {
            let presentation = self.invocation.slots.get(&slot)?;
            // User-authored labels render verbatim: passing them through the
            // localization table would translate any label that happens to
            // collide with a known key ("Copy" → "Copier" under fr).
            let label = if presentation.literal {
                presentation.label.clone()
            } else if let Some(key) = Action::translation_key_for_label(&presentation.label) {
                rust_i18n::t!(key).into_owned()
            } else {
                presentation.label.clone()
            };
            Some((slot, SharedString::from(label)))
        });
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, colors))
            .collect::<Vec<_>>();

        div()
            .relative()
            .size(px(window))
            .child(
                div()
                    .absolute()
                    .left(px((window - footprint) / 2.0))
                    .top(px((window - footprint) / 2.0))
                    .size(px(footprint))
                    .rounded_full()
                    .bg(FOOTPRINT_FILL),
            )
            .when_some(self.hovered, |ring, slot| {
                ring.child(beam_element(slot, geometry))
            })
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px((window - geometry.cancel) / 2.0))
                    .top(px((window - geometry.cancel) / 2.0))
                    .size(px(geometry.cancel))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(colors.cancel_border)
                    .bg(colors.cancel)
                    .hover(move |cancel| cancel.bg(colors.cancel_hover))
                    // An `svg` paints only with its own text colour; a parent's
                    // does not reach it.
                    .child(
                        svg()
                            .path(RING_CANCEL_ICON)
                            .size(px(geometry.cancel_glyph))
                            .flex_none()
                            .text_color(colors.cancel_glyph),
                    ),
            )
            .when_some(hovered_label, |ring, (slot, label)| {
                ring.child(label_element(
                    LabelPlace::of(slot, geometry),
                    label,
                    geometry,
                ))
            })
    }
}

/// A hovered slot's label, painted over the whole ring.
///
/// GPUI paints a primitive above only those whose bounds it overlaps, and a
/// drop shadow's bounds end where its blur begins: the selected slot's glow
/// reaches past them and would paint over the label's near edge. A layer
/// spanning the panel overlaps the glow, so everything in it lands on top.
fn label_element(place: LabelPlace, label: SharedString, geometry: Geometry) -> impl IntoElement {
    canvas(
        move |panel, window, cx| {
            let mut bubble = label_bubble(place, label).into_any_element();
            let origin = panel.origin + point(px(place.left), px(place.top));
            let space = Size::new(px(place.width), px(LABEL_MAX_HEIGHT));
            bubble.prepaint_as_root(origin, space.into(), window, cx);
            bubble
        },
        |panel, mut bubble, window, cx| {
            window.paint_layer(panel, |window| bubble.paint(window, cx));
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .size(px(geometry.window))
}

/// A label as a speech bubble, kept to the edges of its [`LabelPlace`] that
/// touch the slot. Long labels wrap once and then end in an ellipsis.
fn label_bubble(place: LabelPlace, label: SharedString) -> Div {
    // The tail leaves the bubble on the side facing the slot. It comes after
    // the bubble so it paints over the bubble's shadow, and a bubble that
    // grows downward reverses the column to put the tail on top.
    let points_up = place.vertical == Grow::After;
    let bubble = div()
        .max_w(px(place.width))
        .px(px(LABEL_PADDING_X))
        .py(px(LABEL_PADDING_Y))
        .rounded_lg()
        .bg(LABEL_FILL)
        .shadow(vec![BoxShadow {
            color: LABEL_SHADOW,
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(6.0),
            spread_radius: px(0.0),
            inset: false,
        }])
        .text_xs()
        .line_height(px(LABEL_LINE_HEIGHT))
        .text_color(LABEL_TEXT)
        .line_clamp(LABEL_LINES)
        .text_ellipsis()
        .child(label);
    div()
        .w(px(place.width))
        .h(px(LABEL_MAX_HEIGHT))
        .flex()
        .map(|region| match place.horizontal {
            Grow::Before => region.justify_end(),
            Grow::Centred => region.justify_center(),
            Grow::After => region.justify_start(),
        })
        .map(|region| match place.vertical {
            Grow::Before => region.items_end(),
            Grow::Centred => region.items_center(),
            Grow::After => region.items_start(),
        })
        .child(
            div()
                .flex()
                .map(|column| {
                    if points_up {
                        column.flex_col_reverse()
                    } else {
                        column.flex_col()
                    }
                })
                .map(|column| match place.horizontal {
                    Grow::Before => column.items_end(),
                    Grow::Centred => column.items_center(),
                    Grow::After => column.items_start(),
                })
                .child(bubble)
                .child(label_tail(place.horizontal, points_up)),
        )
}

/// A label's tail: a triangle whose base sinks a pixel into the bubble, so no
/// seam shows, and whose tip leans toward the slot when the bubble sits off to
/// one side of it.
fn label_tail(horizontal: Grow, points_up: bool) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let (base, tip) = if points_up {
                (bounds.bottom(), bounds.top())
            } else {
                (bounds.top(), bounds.bottom())
            };
            let tip_x = match horizontal {
                Grow::Before => bounds.right(),
                Grow::Centred => bounds.center().x,
                Grow::After => bounds.left(),
            };
            let mut path = PathBuilder::fill();
            path.move_to(point(bounds.left(), base));
            path.line_to(point(bounds.right(), base));
            path.line_to(point(tip_x, tip));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, LABEL_FILL);
            }
        },
    )
    .flex_none()
    .w(px(LABEL_TAIL_WIDTH))
    .h(px(LABEL_TAIL_HEIGHT + 1.0))
    .map(|tail| {
        if points_up {
            tail.mb(px(-1.0))
        } else {
            tail.mt(px(-1.0))
        }
    })
    .map(|tail| match horizontal {
        Grow::Before => tail.mr(px(LABEL_TAIL_INSET)),
        Grow::Centred => tail,
        Grow::After => tail.ml(px(LABEL_TAIL_INSET)),
    })
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let placement = self.placement;
        let center = self
            .pointer
            .read(cx)
            .center_in(window.window_handle().window_id());
        let colors = RingColors::for_appearance(window.appearance());
        let geometry = self.geometry();
        let panel =
            panel_origin(placement, geometry, center, window.viewport_size()).map(|origin| {
                div()
                    .absolute()
                    .left(origin.x)
                    .top(origin.y)
                    .child(self.panel(colors))
            });
        let click_pointer = self.pointer.clone();
        let filled: Vec<_> = self.invocation.slots.keys().copied().collect();

        div()
            .id("ring-root")
            .relative()
            .size_full()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                let heard_in = window.window_handle().window_id();
                if matches!(this.placement, Placement::AtPointer) {
                    this.pointer.update(cx, |pointer, cx| {
                        if pointer.hear(heard_in, event.position) {
                            cx.notify();
                        }
                    });
                }
                let geometry = this.geometry();
                let Some((x, y)) = offset_from_centre(
                    this.placement,
                    geometry,
                    this.pointer.read(cx),
                    window,
                    event.position,
                ) else {
                    return;
                };
                let aimed = aimed_slot(this.invocation.slots.keys().copied(), geometry, x, y);
                if this.hover(aimed) {
                    cx.notify();
                }
            }))
            .children(panel)
            // The view is not borrowed here, so the dismissal can reach every
            // ring window; the click is aimed from where it landed.
            .on_click(move |event: &ClickEvent, window, cx| {
                let aimed = offset_from_centre(
                    placement,
                    geometry,
                    click_pointer.read(cx),
                    window,
                    event.position(),
                )
                .and_then(|(x, y)| aimed_slot(filled.iter().copied(), geometry, x, y));
                let command = match aimed {
                    Some(slot) => OverlayCommand::Activate { session_id, slot },
                    None => OverlayCommand::Cancel { session_id },
                };
                let _ = root_commands.send(command);
                session::dismiss_from_window(cx, window, session_id);
            })
    }
}

/// How far `position` in `window` lies from the ring's centre, or nothing
/// while this window does not draw the panel.
fn offset_from_centre(
    placement: Placement,
    geometry: Geometry,
    pointer: &RingPointer,
    window: &Window,
    position: Point<Pixels>,
) -> Option<(f32, f32)> {
    let center = pointer.center_in(window.window_handle().window_id());
    let origin = panel_origin(placement, geometry, center, window.viewport_size())?;
    let half = px(geometry.window / 2.0);
    Some((
        (position.x - origin.x - half).as_f32(),
        (position.y - origin.y - half).as_f32(),
    ))
}

/// Appearance shared by the platform-specific placement paths.
pub(crate) fn ring_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some(RING_APP_ID.to_string()),
        ..WindowOptions::default()
    }
}

/// Top-left of the ring panel within its window, or nothing while this window
/// does not draw it.
///
/// `center` is where this window heard the pointer, if it draws the panel.
/// `viewport` is the pointer's space. Display bounds are logical pixels and
/// disagree with it under fractional scaling, so the clamp cannot use them.
fn panel_origin(
    placement: Placement,
    geometry: Geometry,
    center: Option<Point<Pixels>>,
    viewport: Size<Pixels>,
) -> Option<Point<Pixels>> {
    let center = match placement {
        Placement::Window => return Some(Point::default()),
        Placement::AtPointer => center?,
    };
    let size = Size::new(px(geometry.window), px(geometry.window));
    Some(clamp_window_origin(
        point(center.x - size.width / 2.0, center.y - size.height / 2.0),
        size,
        Bounds::new(Point::default(), viewport),
    ))
}

pub(crate) fn clamp_window_origin(
    desired: Point<Pixels>,
    window_size: Size<Pixels>,
    display: Bounds<Pixels>,
) -> Point<Pixels> {
    let max = point(
        display.right() - window_size.width,
        display.bottom() - window_size.height,
    );
    desired.clamp(&display.origin, &max)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
    use openlogi_core::binding::ActionRingIcon;
    use openlogi_ipc::ActionRingPresentation;

    use super::*;

    fn window(id: u64) -> WindowId {
        WindowId::from(id)
    }

    #[test]
    fn overlay_origin_is_clamped_to_the_display() {
        let display = Bounds::new(point(px(100.0), px(50.0)), Size::new(px(800.0), px(600.0)));
        let size = Size::new(px(400.0), px(400.0));
        assert_eq!(
            clamp_window_origin(point(px(-50.0), px(-50.0)), size, display),
            point(px(100.0), px(50.0))
        );
        assert_eq!(
            clamp_window_origin(point(px(700.0), px(500.0)), size, display),
            point(px(500.0), px(250.0))
        );
    }

    const MEDIUM: Geometry = Geometry::of(ActionRingSize::Medium);

    #[test]
    fn a_window_sized_ring_fills_its_window() {
        assert_eq!(
            panel_origin(
                Placement::Window,
                MEDIUM,
                None,
                Size::new(px(360.0), px(360.0))
            ),
            Some(Point::default())
        );
    }

    #[test]
    fn a_display_sized_ring_draws_nothing_until_the_pointer_is_known() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer,
                MEDIUM,
                None,
                Size::new(px(1920.0), px(1080.0))
            ),
            None
        );
    }

    #[test]
    fn a_display_sized_ring_centers_on_the_pointer() {
        let at_pointer = |size| {
            panel_origin(
                Placement::AtPointer,
                Geometry::of(size),
                Some(point(px(900.0), px(500.0))),
                Size::new(px(1920.0), px(1080.0)),
            )
        };
        assert_eq!(
            at_pointer(ActionRingSize::Medium),
            Some(point(px(720.0), px(320.0)))
        );
        // A large ring's panel is 400 pt across.
        assert_eq!(
            at_pointer(ActionRingSize::Large),
            Some(point(px(700.0), px(300.0)))
        );
    }

    #[test]
    fn a_display_sized_ring_stays_whole_against_an_edge() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer,
                MEDIUM,
                Some(point(px(4.0), px(1076.0))),
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(0.0), px(720.0)))
        );
    }

    #[test]
    fn no_window_draws_the_panel_before_one_hears_the_pointer() {
        assert_eq!(RingPointer::default().center_in(window(1)), None);
    }

    #[test]
    fn only_the_window_that_heard_the_pointer_draws_the_panel() {
        let mut pointer = RingPointer::default();
        assert!(pointer.hear(window(1), point(px(900.0), px(500.0))));
        assert_eq!(
            pointer.center_in(window(1)),
            Some(point(px(900.0), px(500.0)))
        );
        assert_eq!(pointer.center_in(window(2)), None);
    }

    #[test]
    fn the_panel_stays_where_its_window_first_heard_the_pointer() {
        let mut pointer = RingPointer::default();
        pointer.hear(window(1), point(px(900.0), px(500.0)));
        assert!(
            !pointer.hear(window(1), point(px(960.0), px(540.0))),
            "a later move in the drawing window does not move the panel"
        );
        assert_eq!(
            pointer.center_in(window(1)),
            Some(point(px(900.0), px(500.0)))
        );
    }

    #[test]
    fn the_window_that_hears_the_pointer_last_takes_the_panel() {
        // GNOME stacks the ring's windows on one display, and the one mapped
        // on top hears the pointer after the one below it (#1901).
        let (below, above) = (window(1), window(2));
        let mut pointer = RingPointer::default();
        pointer.hear(below, point(px(300.0), px(200.0)));
        assert!(pointer.hear(above, point(px(900.0), px(500.0))));
        assert_eq!(
            pointer.center_in(below),
            None,
            "the covered window stops drawing"
        );
        assert_eq!(pointer.center_in(above), Some(point(px(900.0), px(500.0))));
        assert!(
            pointer.hear(below, point(px(10.0), px(20.0))),
            "a window takes the panel back when it hears the pointer again"
        );
        assert_eq!(pointer.center_in(above), None);
        assert_eq!(pointer.center_in(below), Some(point(px(10.0), px(20.0))));
    }

    fn invocation() -> ActionRingInvocation {
        sized_invocation(ActionRingSize::Medium)
    }

    fn sized_invocation(size: ActionRingSize) -> ActionRingInvocation {
        ActionRingInvocation {
            session_id: 7,
            slots: [(
                ActionRingSlot::Top,
                ActionRingPresentation {
                    label: "Copy".to_string(),
                    literal: true,
                    icon: ActionRingIcon::Copy,
                },
            )]
            .into(),
            language: None,
            size,
        }
    }

    /// Where the `Top` slot's centre lands when the panel centres on `center`
    /// well away from every edge.
    fn top_slot_at(center: Point<Pixels>) -> Point<Pixels> {
        let (left, top) = ActionRingSlot::Top.placement(MEDIUM.window, MEDIUM.radius, MEDIUM.slot);
        point(
            center.x - px(MEDIUM.window / 2.0) + px(left + MEDIUM.slot / 2.0),
            center.y - px(MEDIUM.window / 2.0) + px(top + MEDIUM.slot / 2.0),
        )
    }

    #[gpui::test]
    fn a_ring_window_that_loses_the_pointer_stops_drawing_and_hovering(cx: &mut TestAppContext) {
        let live = Arc::new(ClickAwaySession::new());
        let (commands, mut sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let open = |cx: &mut TestAppContext| -> WindowHandle<RingView> {
            let (commands, pointer, live) = (commands.clone(), pointer.clone(), Arc::clone(&live));
            cx.add_window(move |window, cx| {
                RingView::new(
                    invocation(),
                    commands,
                    &live,
                    Placement::AtPointer,
                    pointer,
                    window,
                    cx,
                )
            })
        };
        let below = open(cx);
        let above = open(cx);
        let center = point(px(600.0), px(500.0));
        let below_view = below.update(cx, |_, _, cx| cx.entity()).unwrap();
        let below_redraws = Rc::new(Cell::new(0_u32));
        cx.update(|cx| {
            let redraws = Rc::clone(&below_redraws);
            cx.observe(&below_view, move |_, _| redraws.set(redraws.get() + 1))
                .detach();
        });

        let mut below_cx = VisualTestContext::from_window(below.into(), cx);
        below_cx.simulate_mouse_move(center, None, Modifiers::default());
        below_cx.simulate_mouse_move(top_slot_at(center), None, Modifiers::default());
        assert_eq!(
            below.read_with(cx, |view, _| view.hovered).unwrap(),
            Some(ActionRingSlot::Top),
            "the window that heard the pointer draws the panel"
        );
        assert!(matches!(
            sent.try_recv(),
            Ok(OverlayCommand::Hover {
                session_id: 7,
                slot: Some(ActionRingSlot::Top)
            })
        ));

        let mut above_cx = VisualTestContext::from_window(above.into(), cx);
        let redraws_before = below_redraws.get();
        above_cx.simulate_mouse_move(center, None, Modifiers::default());
        let below_id = below.window_id();
        let above_id = above.window_id();
        assert_eq!(
            pointer.read_with(cx, |pointer, _| pointer.center_in(below_id)),
            None
        );
        assert_eq!(
            pointer.read_with(cx, |pointer, _| pointer.center_in(above_id)),
            Some(center)
        );
        assert_eq!(
            below.read_with(cx, |view, _| view.hovered).unwrap(),
            None,
            "the covered window no longer has a slot under the pointer"
        );
        assert!(
            below_redraws.get() > redraws_before,
            "the covered window redraws without its panel"
        );

        above_cx.simulate_mouse_move(top_slot_at(center), None, Modifiers::default());
        assert_eq!(
            above.read_with(cx, |view, _| view.hovered).unwrap(),
            Some(ActionRingSlot::Top),
            "the window on top draws a working panel"
        );
    }

    /// The pointer chooses by direction at every size: past a slot still
    /// chooses it, the dead zone and an empty slot's sector choose nothing,
    /// and a click lands on what it is aimed at.
    #[gpui::test]
    fn the_pointer_aims_at_a_slot_by_direction(cx: &mut TestAppContext) {
        for size in ActionRingSize::ALL {
            let live = Arc::new(ClickAwaySession::new());
            let (commands, mut sent) = mpsc::unbounded_channel();
            let pointer = cx.new(|_| RingPointer::default());
            let ring = cx.add_window(move |window, cx| {
                RingView::new(
                    sized_invocation(size),
                    commands,
                    &live,
                    Placement::Window,
                    pointer,
                    window,
                    cx,
                )
            });
            let mut ring_cx = VisualTestContext::from_window(ring.into(), cx);
            let geometry = Geometry::of(size);
            let centre = geometry.window / 2.0;
            let at = |x: f32, y: f32| point(px(centre + x), px(centre + y));
            let hovered =
                |cx: &mut VisualTestContext| ring.read_with(cx, |view, _| view.hovered).unwrap();
            let hover = |slot| OverlayCommand::Hover {
                session_id: 7,
                slot,
            };

            // Well past the top slot and off its axis, but inside its sector.
            ring_cx.simulate_mouse_move(at(30.0, -150.0), None, Modifiers::default());
            assert_eq!(hovered(&mut ring_cx), Some(ActionRingSlot::Top), "{size:?}");
            assert_eq!(sent.try_recv().ok(), Some(hover(Some(ActionRingSlot::Top))));

            // Straight up toward the top slot, just inside this size's dead zone.
            let edge = geometry.dead_zone;
            ring_cx.simulate_mouse_move(at(0.0, 2.0 - edge), None, Modifiers::default());
            assert_eq!(
                hovered(&mut ring_cx),
                None,
                "{size:?}: the centre chooses nothing"
            );
            assert_eq!(sent.try_recv().ok(), Some(hover(None)));
            ring_cx.simulate_mouse_move(at(0.0, -2.0 - edge), None, Modifiers::default());
            assert_eq!(hovered(&mut ring_cx), Some(ActionRingSlot::Top), "{size:?}");
            assert_eq!(sent.try_recv().ok(), Some(hover(Some(ActionRingSlot::Top))));

            // Right has no action in this ring.
            ring_cx.simulate_mouse_move(at(geometry.radius, 0.0), None, Modifiers::default());
            assert_eq!(hovered(&mut ring_cx), None, "{size:?}");
            assert_eq!(sent.try_recv().ok(), Some(hover(None)));

            ring_cx.simulate_click(at(10.0, -60.0), Modifiers::default());
            let mut last = None;
            while let Ok(command) = sent.try_recv() {
                last = Some(command);
            }
            assert_eq!(
                last,
                Some(OverlayCommand::Activate {
                    session_id: 7,
                    slot: ActionRingSlot::Top
                }),
                "{size:?}"
            );
        }
    }

    /// At every ring size, a hovered slot's label, at its longest and tallest,
    /// stays in the window and clear of every slot, including the hovered one
    /// at its grown size.
    #[test]
    fn a_hovered_slots_label_stays_in_the_window_and_off_every_slot() {
        for ring in ActionRingSize::ALL {
            labels_fit(Geometry::of(ring));
        }
    }

    #[track_caller]
    fn labels_fit(geometry: Geometry) {
        let window = geometry.window;
        let overlaps = |place: LabelPlace, slot: ActionRingSlot, size: f32| {
            let (left, top) = slot.placement(window, geometry.radius, size);
            let (cx, cy, r) = (left + size / 2.0, top + size / 2.0, size / 2.0);
            let nearest_x = cx.clamp(place.left, place.left + place.width);
            let nearest_y = cy.clamp(place.top, place.top + LABEL_MAX_HEIGHT);
            (cx - nearest_x).hypot(cy - nearest_y) < r
        };
        for hovered in ActionRingSlot::ALL {
            let place = LabelPlace::of(hovered, geometry);
            assert!(
                place.left >= 0.0
                    && place.top >= 0.0
                    && place.left + place.width <= window
                    && place.top + LABEL_MAX_HEIGHT <= window,
                "{hovered:?}'s label leaves the {window} pt window: {place:?}"
            );
            assert!(
                place.width >= 90.0,
                "{hovered:?}'s label has room for only {} pt in the {window} pt window",
                place.width
            );
            for slot in ActionRingSlot::ALL {
                let size = if slot == hovered {
                    geometry.selected_slot
                } else {
                    geometry.slot
                };
                assert!(
                    !overlaps(place, slot, size),
                    "{hovered:?}'s label covers {slot:?} in the {window} pt window: {place:?}"
                );
            }
        }
    }

    #[test]
    fn overlay_origin_stays_cursor_centered_away_from_edges() {
        let display = Bounds::new(Point::default(), Size::new(px(1600.0), px(1000.0)));
        let desired = point(px(600.0), px(300.0));
        assert_eq!(
            clamp_window_origin(desired, Size::new(px(400.0), px(400.0)), display),
            desired
        );
    }
}

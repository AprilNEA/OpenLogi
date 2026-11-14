//! The dialpad's surface in the Keys view. On a render OpenLogi measured
//! ([`DialpadRender`]) the picture itself is the selector: every key, both
//! halves of the dial and of the roller are click targets laid over the
//! parts they are, outlined while hovered and ringed while selected. Without
//! one, a drawing of the device built from its model stands in — the large
//! dial, the small roller, and the four keys where the registry places them,
//! laid out like the MX Creative Dialpad's chassis: the two upper keys side
//! by side at the top left, the roller at the top right, the dial below
//! them, and the two lower keys in the bottom corners beside the dial.
//!
//! Each turning direction binds on its own, like the thumb wheel's: the dial
//! carries a counter-clockwise and a clockwise half, the roller an up and a
//! down half. Every part is a click target that opens the same panel as any
//! other key. Selection, hover, and the panel are the parent view's; this
//! only draws the dialpad and reports clicks and hovers back to it.

use gpui::{
    App, Bounds, Entity, Hsla, InteractiveElement, IntoElement, ParentElement, PathBuilder, Pixels,
    Point, RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled, Window, canvas,
    div, point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use openlogi_core::binding::{ButtonId, Rotary};
use openlogi_core::hid::{DialpadDescriptor, DialpadKeyPosition};

use super::key_points::{KEYCAP_H, KEYCAP_W, KeyPoint, KeySlotLayout, KeyTarget};
use super::{
    FunctionRowView, KeySlot, KeyState, KeySurfaceStyle as _, accent_alpha, binding_line,
    device_highlight, device_scrim, image_or_fallback,
};
use crate::services::assets::ResolvedAsset;
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};

/// The canvas width, in CSS px: the lower keys sit in its corners.
const CANVAS_W: f32 = 380.;
/// The dial's diameter, centred on the canvas.
const DIAL_D: f32 = 224.;
/// One half of the dial: a turning direction's click target inside the
/// dial's ring.
const DIAL_HALF_W: f32 = 88.;
const DIAL_HALF_H: f32 = 108.;
/// The gap between the dial's two halves.
const DIAL_SPLIT: f32 = 12.;
/// The roller's body, in the top right corner; it sets the top band's height.
const ROLLER_W: f32 = 96.;
const ROLLER_H: f32 = 96.;
/// The inset of the roller's halves inside its body, and the gap between them.
const ROLLER_INSET: f32 = 4.;
/// The gap between the top band and the dial.
const ROW_GAP: f32 = 16.;
/// The gap between the two upper keys.
const KEY_GAP: f32 = 12.;
/// How far below the dial's centre the lower keys start: far enough down
/// that the dial's ring has curved away from their inner corners.
const LOWER_KEY_DROP: f32 = 64.;
/// The corner radius of a key, and of a half's inner corners.
const KEY_RADIUS: f32 = 8.;

/// A rectangle on the dialpad canvas, in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PartRect {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}

impl PartRect {
    /// The rectangle's centre.
    pub(super) fn center(self) -> (f32, f32) {
        (self.left + self.w / 2., self.top + self.h / 2.)
    }

    /// Whether `other` overlaps this rectangle.
    #[cfg(test)]
    pub(super) fn overlaps(self, other: Self) -> bool {
        self.left < other.left + other.w
            && other.left < self.left + self.w
            && self.top < other.top + other.h
            && other.top < self.top + self.h
    }
}

/// One bindable part of the dialpad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DialpadPart {
    /// A key, where the registry places it.
    Key(DialpadKeyPosition),
    /// The dial turned counter-clockwise.
    DialLeft,
    /// The dial turned clockwise.
    DialRight,
    /// The roller rolled up.
    RollerUp,
    /// The roller rolled down.
    RollerDown,
}

/// The parts in reading order: the top keys, the dial and roller between
/// them, then the bottom keys.
const READING_ORDER: [DialpadPart; 8] = [
    DialpadPart::Key(DialpadKeyPosition::TopLeft),
    DialpadPart::Key(DialpadKeyPosition::TopRight),
    DialpadPart::DialLeft,
    DialpadPart::DialRight,
    DialpadPart::RollerUp,
    DialpadPart::RollerDown,
    DialpadPart::Key(DialpadKeyPosition::BottomLeft),
    DialpadPart::Key(DialpadKeyPosition::BottomRight),
];

impl DialpadPart {
    /// What this part binds on `dialpad`: a key's control, or a turning
    /// direction of a rotary control. `None` for a key position the dialpad
    /// has no key at.
    fn target(self, dialpad: &DialpadDescriptor) -> Option<KeyTarget> {
        let button = match self {
            Self::Key(position) => dialpad
                .keys
                .iter()
                .find(|key| key.position == position)
                .map(|key| ButtonId::control(key.cid))?,
            Self::DialLeft => Rotary::Dial.negative(),
            Self::DialRight => Rotary::Dial.positive(),
            Self::RollerUp => Rotary::Roller.positive(),
            Self::RollerDown => Rotary::Roller.negative(),
        };
        Some(KeyTarget::Control(button))
    }

    /// The part `target` binds on `dialpad` — the inverse of
    /// [`Self::target`] — or `None` when it is not one of the dialpad's.
    pub(super) fn of(target: &KeyTarget, dialpad: &DialpadDescriptor) -> Option<Self> {
        READING_ORDER
            .into_iter()
            .find(|part| part.target(dialpad).as_ref() == Some(target))
    }

    /// The part's localized name.
    fn legend(self) -> String {
        let key = match self {
            Self::Key(DialpadKeyPosition::TopLeft) => "dialpad.key_top_left",
            Self::Key(DialpadKeyPosition::TopRight) => "dialpad.key_top_right",
            Self::Key(DialpadKeyPosition::BottomLeft) => "dialpad.key_bottom_left",
            Self::Key(DialpadKeyPosition::BottomRight) => "dialpad.key_bottom_right",
            Self::DialLeft => Rotary::Dial.negative().translation_key(),
            Self::DialRight => Rotary::Dial.positive().translation_key(),
            Self::RollerUp => Rotary::Roller.positive().translation_key(),
            Self::RollerDown => Rotary::Roller.negative().translation_key(),
        };
        rust_i18n::t!(key).into_owned()
    }

    /// Where the part is drawn on the dialpad canvas.
    pub(super) fn rect(self) -> PartRect {
        let dial = dial_rect();
        let roller = roller_rect();
        let roller_half_h = (ROLLER_H - 3. * ROLLER_INSET) / 2.;
        let dial_center_x = dial.left + DIAL_D / 2.;
        let dial_half_top = dial.top + (DIAL_D - DIAL_HALF_H) / 2.;
        let key_at = |left: f32, top: f32| PartRect {
            left,
            top,
            w: KEYCAP_W,
            h: KEYCAP_H,
        };
        let upper_top = (ROLLER_H - KEYCAP_H) / 2.;
        let lower_top = dial.top + DIAL_D / 2. + LOWER_KEY_DROP;
        match self {
            Self::Key(DialpadKeyPosition::TopLeft) => key_at(0., upper_top),
            Self::Key(DialpadKeyPosition::TopRight) => key_at(KEYCAP_W + KEY_GAP, upper_top),
            Self::Key(DialpadKeyPosition::BottomLeft) => key_at(0., lower_top),
            Self::Key(DialpadKeyPosition::BottomRight) => key_at(CANVAS_W - KEYCAP_W, lower_top),
            Self::DialLeft => PartRect {
                left: dial_center_x - DIAL_SPLIT / 2. - DIAL_HALF_W,
                top: dial_half_top,
                w: DIAL_HALF_W,
                h: DIAL_HALF_H,
            },
            Self::DialRight => PartRect {
                left: dial_center_x + DIAL_SPLIT / 2.,
                top: dial_half_top,
                w: DIAL_HALF_W,
                h: DIAL_HALF_H,
            },
            Self::RollerUp => PartRect {
                left: roller.left + ROLLER_INSET,
                top: roller.top + ROLLER_INSET,
                w: ROLLER_W - 2. * ROLLER_INSET,
                h: roller_half_h,
            },
            Self::RollerDown => PartRect {
                left: roller.left + ROLLER_INSET,
                top: roller.top + 2. * ROLLER_INSET + roller_half_h,
                w: ROLLER_W - 2. * ROLLER_INSET,
                h: roller_half_h,
            },
        }
    }

    /// Corner radii — top left, top right, bottom right, bottom left. A
    /// half's outer corners follow the curve of the body it sits in; GPUI
    /// clamps each to half the part's shorter side.
    fn radii(self) -> [f32; 4] {
        let (k, outer) = (KEY_RADIUS, DIAL_D / 2.);
        match self {
            Self::Key(_) => [k; 4],
            Self::DialLeft => [outer, k, k, outer],
            Self::DialRight => [k, outer, outer, k],
            Self::RollerUp => [outer, outer, k, k],
            Self::RollerDown => [k, k, outer, outer],
        }
    }

    /// The glyph drawn on the part: a ring for one of the round keys, or the
    /// icon of a turning direction.
    fn glyph(self) -> PartGlyph {
        match self {
            Self::Key(_) => PartGlyph::RoundKey,
            Self::DialLeft => PartGlyph::Icon("action-icons/rotate-ccw.svg"),
            Self::DialRight => PartGlyph::Icon("action-icons/rotate-cw.svg"),
            Self::RollerUp => PartGlyph::Icon("action-icons/chevrons-up.svg"),
            Self::RollerDown => PartGlyph::Icon("action-icons/chevrons-down.svg"),
        }
    }
}

/// What a part shows above its binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PartGlyph {
    /// The key's round cap, outlined.
    RoundKey,
    Icon(&'static str),
}

/// The dial's ring on the canvas, centred below the top band.
pub(super) fn dial_rect() -> PartRect {
    PartRect {
        left: (CANVAS_W - DIAL_D) / 2.,
        top: ROLLER_H + ROW_GAP,
        w: DIAL_D,
        h: DIAL_D,
    }
}

/// The roller's body, in the top right corner.
pub(super) fn roller_rect() -> PartRect {
    PartRect {
        left: CANVAS_W - ROLLER_W,
        top: 0.,
        w: ROLLER_W,
        h: ROLLER_H,
    }
}

/// The canvas the dialpad's parts are placed on, in CSS px: tall enough
/// for the dial or for the lower keys below its centre, whichever reaches
/// further down.
pub(super) fn dialpad_canvas() -> (f32, f32) {
    let dial = dial_rect();
    let below_dial = dial.top + DIAL_D;
    let below_keys = dial.top + DIAL_D / 2. + LOWER_KEY_DROP + KEYCAP_H;
    (CANVAS_W, below_dial.max(below_keys))
}

/// A dialpad render OpenLogi measured, in that render's own pixels. Logi's
/// depot marks the dial and the roller but neither turning direction nor the
/// keys' outlines, so OpenLogi carries the parts' geometry for each render
/// the tab may show. A render is recognised by its depot and exact size: one
/// of any other size is no longer the one measured, and the drawing stands
/// in rather than landing the parts off the device.
#[derive(Debug, PartialEq)]
pub(super) struct DialpadRender {
    depot: &'static str,
    /// The PNG's size, in px.
    size: (u32, u32),
    /// Each key's centre; the keys are round.
    keys: [(DialpadKeyPosition, (f32, f32)); 4],
    key_d: f32,
    /// The roller's centre, size, and corner radius.
    roller: (f32, f32),
    roller_size: (f32, f32),
    roller_radius: f32,
    /// The dial's centre and diameter.
    dial: (f32, f32),
    dial_d: f32,
}

/// The MX Creative Dialpad's product render, `front_core.png`. Measured
/// 2026-11-03: the keys span 85–258 px and 285–458 px across the top and
/// 86–258 px and 866–1038 px across the bottom; the roller 713–990 px
/// across, 98–248 px down; the dial 267–857 px across, 332–922 px down. The
/// depot's own markers agree: the dial's left edge at 22 %/54 % and the
/// roller at 85.5 %/13 %.
const DIALPAD_FRONT_CORE: DialpadRender = DialpadRender {
    depot: "mx_creative_dialpad",
    size: (1124, 1105),
    keys: [
        (DialpadKeyPosition::TopLeft, (172., 172.)),
        (DialpadKeyPosition::TopRight, (372., 172.)),
        (DialpadKeyPosition::BottomLeft, (172., 930.)),
        (DialpadKeyPosition::BottomRight, (952., 930.)),
    ],
    key_d: 172.,
    roller: (851., 173.),
    roller_size: (277., 150.),
    roller_radius: 14.,
    dial: (562., 627.),
    dial_d: 590.,
};

/// Every dialpad render OpenLogi measured.
const DIALPAD_RENDERS: [&DialpadRender; 1] = [&DIALPAD_FRONT_CORE];

/// The measured render `asset` is, if any.
pub(super) fn dialpad_render(asset: &ResolvedAsset) -> Option<&'static DialpadRender> {
    DIALPAD_RENDERS.into_iter().find(|render| {
        asset.depot == render.depot && (asset.png_width, asset.png_height) == render.size
    })
}

impl DialpadRender {
    /// The render's size, in px.
    #[expect(
        clippy::cast_precision_loss,
        reason = "render dimensions are a few thousand pixels at most"
    )]
    pub(super) fn size(&self) -> (f32, f32) {
        (self.size.0 as f32, self.size.1 as f32)
    }

    /// The dial's bounding square, in render px.
    pub(super) fn dial_rect(&self) -> PartRect {
        let (x, y) = self.dial;
        let r = self.dial_d / 2.;
        PartRect {
            left: x - r,
            top: y - r,
            w: self.dial_d,
            h: self.dial_d,
        }
    }

    /// Where `part` lies on the render, in its px: a key's bounding square,
    /// a half of the dial's bounding square, a half of the roller.
    pub(super) fn rect(&self, part: DialpadPart) -> PartRect {
        let dial = self.dial_rect();
        let (rx, ry) = self.roller;
        let (rw, rh) = self.roller_size;
        match part {
            DialpadPart::Key(position) => {
                let (x, y) = self
                    .keys
                    .iter()
                    .find(|(at, _)| *at == position)
                    .map_or((0., 0.), |(_, centre)| *centre);
                PartRect {
                    left: x - self.key_d / 2.,
                    top: y - self.key_d / 2.,
                    w: self.key_d,
                    h: self.key_d,
                }
            }
            DialpadPart::DialLeft => PartRect {
                w: dial.w / 2.,
                ..dial
            },
            DialpadPart::DialRight => PartRect {
                left: dial.left + dial.w / 2.,
                w: dial.w / 2.,
                ..dial
            },
            DialpadPart::RollerUp => PartRect {
                left: rx - rw / 2.,
                top: ry - rh / 2.,
                w: rw,
                h: rh / 2.,
            },
            DialpadPart::RollerDown => PartRect {
                left: rx - rw / 2.,
                top: ry,
                w: rw,
                h: rh / 2.,
            },
        }
    }
}

/// The dialpad's bindable parts in reading order, each at its centre: on
/// the measured `render` when there is one, else on [`dialpad_canvas`].
pub(super) fn dialpad_slots(
    dialpad: &DialpadDescriptor,
    render: Option<&DialpadRender>,
) -> Vec<KeySlotLayout> {
    let (w, h) = render.map_or_else(dialpad_canvas, DialpadRender::size);
    READING_ORDER
        .iter()
        .filter_map(|part| {
            let rect = render.map_or_else(|| part.rect(), |render| render.rect(*part));
            let (x, y) = rect.center();
            Some(KeySlotLayout {
                target: part.target(dialpad)?,
                legend: part.legend(),
                point: KeyPoint {
                    x_frac: x / w,
                    y_frac: y / h,
                },
            })
        })
        .collect()
}

/// The dialpad drawn from its model beside its photo, when one resolved:
/// the dial's ring split into its two turning directions, the roller's body
/// split into up and down, and the four keys around them. Each part carries
/// its binding, opens the panel on click, and shares the keycaps' selection
/// and hover treatment.
#[derive(IntoElement)]
pub(super) struct DialpadPane {
    slots: Vec<KeySlot>,
    dialpad: &'static DialpadDescriptor,
    image: Option<std::path::PathBuf>,
    photo: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl DialpadPane {
    pub(super) fn new(
        slots: Vec<KeySlot>,
        dialpad: &'static DialpadDescriptor,
        image: Option<std::path::PathBuf>,
        photo: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
            dialpad,
            image,
            photo,
            selected: None,
            hovered: None,
            view,
        }
    }

    #[must_use]
    pub(super) fn selected(mut self, selected: impl Into<Option<usize>>) -> Self {
        self.selected = selected.into();
        self
    }

    #[must_use]
    pub(super) fn hovered(mut self, hovered: impl Into<Option<usize>>) -> Self {
        self.hovered = hovered.into();
        self
    }
}

impl RenderOnce for DialpadPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (w, h) = dialpad_canvas();
        let (selected, hovered) = (self.selected, self.hovered);
        let pal = theme::palette(cx);
        let (photo_w, photo_h) = self.photo;
        let dialpad = self.dialpad;
        let view = self.view;
        let body = |rect: PartRect| {
            div()
                .absolute()
                .left(px(rect.left))
                .top(px(rect.top))
                .w(px(rect.w))
                .h(px(rect.h))
                .rounded_full()
                .border_2()
                .border_color(pal.border)
        };
        let parts = div()
            .relative()
            .w(px(w))
            .h(px(h))
            .child(body(dial_rect()))
            .child(body(roller_rect()))
            .children(self.slots.into_iter().filter_map(move |slot| {
                let part = DialpadPart::of(&slot.target, dialpad)?;
                Some(PartControl {
                    state: KeyState::of(slot.idx, selected, hovered),
                    part,
                    slot,
                    view: view.clone(),
                })
            }));
        h_flex()
            .items_center()
            .gap_6()
            .when_some(self.image, |row, path| {
                row.child(image_or_fallback(Some(path), photo_w, photo_h, &pal))
            })
            .child(
                div()
                    .p_4()
                    .rounded(pal.card_radius)
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .child(parts),
            )
    }
}

/// One bindable part: its glyph and binding, its click and hover behaviour.
#[derive(IntoElement)]
struct PartControl {
    slot: KeySlot,
    part: DialpadPart,
    state: KeyState,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for PartControl {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let rect = self.part.rect();
        let [tl, tr, br, bl] = self.part.radii().map(px);
        let idx = self.slot.idx;
        let state = self.state;
        let highlighted = state.is_highlighted();
        let tooltip: SharedString = format!("{} · {}", self.slot.label, self.slot.binding).into();
        let a11y_label = tooltip.clone();
        let foreground = if highlighted {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_primary
        };
        let glyph = match self.part.glyph() {
            PartGlyph::RoundKey => div()
                .size(px(14.))
                .rounded_full()
                .border_2()
                .border_color(foreground)
                .into_any_element(),
            PartGlyph::Icon(path) => svg()
                .path(path)
                .size(px(18.))
                .flex_none()
                .text_color(foreground)
                .into_any_element(),
        };
        // The outer curve of a half leaves less room for text than its box.
        let inset = match self.part {
            DialpadPart::Key(_) => 8.,
            DialpadPart::DialLeft
            | DialpadPart::DialRight
            | DialpadPart::RollerUp
            | DialpadPart::RollerDown => 16.,
        };
        let view_hover = self.view.clone();
        let view_click = self.view;

        v_flex()
            .id(("dialpad-part", idx))
            .role(Role::Button)
            .aria_label(a11y_label)
            .debug_selector(move || format!("dialpad-part-{idx}"))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .absolute()
            .left(px(rect.left))
            .top(px(rect.top))
            .w(px(rect.w))
            .h(px(rect.h))
            .items_center()
            .justify_center()
            .gap(px(2.))
            .rounded_tl(tl)
            .rounded_tr(tr)
            .rounded_br(br)
            .rounded_bl(bl)
            .key_state_surface(state, &pal)
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if highlighted {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .child(glyph)
            .child(binding_line(
                self.slot.binding,
                Some(self.slot.binding_icon),
                (rect.w - inset).max(0.),
                highlighted,
                &pal,
            ))
            .on_hover(move |hovered, _window, cx| {
                let next = (*hovered).then_some(idx);
                view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
            })
            .on_click(move |_ev, _window, cx| {
                view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
            })
    }
}

/// Gap between a key's edge and its ring on the photo, in px.
const PHOTO_RING_GAP: f32 = 3.;
const PHOTO_RING_W: f32 = 2.;
/// The selected part's heavier ring on the photo, in px.
const PHOTO_SELECTED_RING_W: f32 = 3.;

/// The badge carrying a small part's bound action on the photo, in px.
const BADGE: f32 = 22.;
const BADGE_ICON: f32 = 12.;

/// The dialpad's render with its parts mapped onto it: the picture itself
/// is the selector. Each part is its own click and hover target, invisible
/// at rest, outlined while hovered, and ringed in the accent while selected;
/// the dial's halves carry their turning glyph and binding inside, a key or
/// a roller half only a badge with its bound action's icon.
#[derive(IntoElement)]
pub(super) struct DialpadPhotoPane {
    slots: Vec<KeySlot>,
    dialpad: &'static DialpadDescriptor,
    render: &'static DialpadRender,
    image: Option<std::path::PathBuf>,
    render_size: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl DialpadPhotoPane {
    pub(super) fn new(
        slots: Vec<KeySlot>,
        (dialpad, render): (&'static DialpadDescriptor, &'static DialpadRender),
        image: Option<std::path::PathBuf>,
        render_size: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
            dialpad,
            render,
            image,
            render_size,
            selected: None,
            hovered: None,
            view,
        }
    }

    #[must_use]
    pub(super) fn selected(mut self, selected: impl Into<Option<usize>>) -> Self {
        self.selected = selected.into();
        self
    }

    #[must_use]
    pub(super) fn hovered(mut self, hovered: impl Into<Option<usize>>) -> Self {
        self.hovered = hovered.into();
        self
    }
}

impl RenderOnce for DialpadPhotoPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (img_w, img_h) = self.render_size;
        let scale = img_w / self.render.size().0;
        let (selected, hovered) = (self.selected, self.hovered);
        let (dialpad, render, view) = (self.dialpad, self.render, self.view);
        let pal = theme::palette(cx);
        let parts: Vec<(KeySlot, DialpadPart, KeyState)> = self
            .slots
            .into_iter()
            .filter_map(|slot| {
                let part = DialpadPart::of(&slot.target, dialpad)?;
                let state = KeyState::of(slot.idx, selected, hovered);
                Some((slot, part, state))
            })
            .collect();
        let half_state = |half: DialpadPart| {
            parts
                .iter()
                .find(|(_, part, _)| *part == half)
                .map_or(KeyState::Rest, |(_, _, state)| *state)
        };
        let dial = DialHighlight {
            rect: render.dial_rect().scaled(scale),
            left: half_state(DialpadPart::DialLeft),
            right: half_state(DialpadPart::DialRight),
        };
        div()
            .relative()
            .w(px(img_w))
            .h(px(img_h))
            .child(image_or_fallback(self.image, img_w, img_h, &pal))
            .child(dial_highlight(dial))
            .children(parts.into_iter().map(move |(slot, part, state)| PhotoPart {
                rect: render.rect(part).scaled(scale),
                radius: render.roller_radius * scale,
                slot,
                part,
                state,
                view: view.clone(),
            }))
    }
}

impl PartRect {
    /// This rectangle with every coordinate multiplied by `scale`.
    fn scaled(self, scale: f32) -> Self {
        Self {
            left: self.left * scale,
            top: self.top * scale,
            w: self.w * scale,
            h: self.h * scale,
        }
    }

    /// This rectangle grown by `by` on every side.
    fn grown(self, by: f32) -> Self {
        Self {
            left: self.left - by,
            top: self.top - by,
            w: self.w + 2. * by,
            h: self.h + 2. * by,
        }
    }
}

/// How each half of the dial is drawn: the halves are half discs, which a
/// rounded box cannot draw (GPUI clamps a corner to half the shorter side),
/// so they are painted as paths over the photo.
#[derive(Clone, Copy, Debug)]
struct DialHighlight {
    /// The dial's bounding square on the drawn render, in px.
    rect: PartRect,
    left: KeyState,
    right: KeyState,
}

fn dial_highlight(dial: DialHighlight) -> impl IntoElement {
    let rect = dial.rect;
    canvas(
        move |_bounds, _, _| dial,
        move |bounds, dial, window, _app| paint_dial_halves(bounds, dial, window),
    )
    .absolute()
    .left(px(rect.left))
    .top(px(rect.top))
    .w(px(rect.w))
    .h(px(rect.h))
}

/// Paint each highlighted half of the dial: a light outline while hovered,
/// the accent ring with its glow and a faint accent fill while selected.
fn paint_dial_halves(bounds: Bounds<Pixels>, dial: DialHighlight, window: &mut Window) {
    let r = dial.rect.w / 2.;
    let centre = bounds.origin + point(px(r), px(r));
    for (state, clockwise) in [(dial.left, false), (dial.right, true)] {
        let half = |builder: PathBuilder| half_disc(builder, centre, r, clockwise);
        match state {
            KeyState::Rest => {}
            KeyState::Hovered => {
                if let Ok(path) = half(PathBuilder::stroke(px(2.))).build() {
                    window.paint_path(path, device_highlight(0.55));
                }
            }
            KeyState::Selected => {
                if let Ok(path) = half(PathBuilder::fill()).build() {
                    window.paint_path(path, accent_alpha(0.16));
                }
                if let Ok(path) = half(PathBuilder::stroke(px(12.))).build() {
                    window.paint_path(path, accent_alpha(0.25));
                }
                if let Ok(path) = half(PathBuilder::stroke(px(3.))).build() {
                    window.paint_path(path, rgb(ACCENT_BLUE));
                }
            }
        }
    }
}

/// The half disc of radius `r` around `centre` on the right (`clockwise`)
/// or the left side: an arc from the top to the bottom, closed along the
/// diameter.
fn half_disc(
    mut builder: PathBuilder,
    centre: Point<Pixels>,
    r: f32,
    clockwise: bool,
) -> PathBuilder {
    builder.move_to(centre - point(px(0.), px(r)));
    builder.arc_to(
        point(px(r), px(r)),
        px(0.),
        false,
        clockwise,
        centre + point(px(0.), px(r)),
    );
    builder.close();
    builder
}

/// One part mapped onto the dialpad render.
#[derive(IntoElement)]
struct PhotoPart {
    slot: KeySlot,
    part: DialpadPart,
    state: KeyState,
    /// The part's box on the drawn render, in px.
    rect: PartRect,
    /// The roller's corner radius on the drawn render, in px.
    radius: f32,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for PhotoPart {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let idx = self.slot.idx;
        let state = self.state;
        let tooltip: SharedString = format!("{} · {}", self.slot.label, self.slot.binding).into();
        let a11y_label = tooltip.clone();
        let ring = match state {
            KeyState::Selected => rgb(ACCENT_BLUE).into(),
            KeyState::Hovered => device_highlight(0.55),
            KeyState::Rest => device_scrim(0.),
        };
        let (rect, radii) = match self.part {
            // A key's ring sits one gap outside its round cap.
            DialpadPart::Key(_) => {
                let rect = self.rect.grown(PHOTO_RING_GAP + PHOTO_RING_W);
                (rect, [rect.w / 2.; 4])
            }
            DialpadPart::RollerUp => (self.rect, [self.radius, self.radius, 0., 0.]),
            DialpadPart::RollerDown => (self.rect, [0., 0., self.radius, self.radius]),
            // The dial's halves are painted by `dial_highlight`.
            DialpadPart::DialLeft | DialpadPart::DialRight => (self.rect, [0.; 4]),
        };
        let [tl, tr, br, bl] = radii.map(px);
        let content = photo_part_content(
            self.part,
            (self.slot.binding.clone(), Some(self.slot.binding_icon)),
            state,
            rect.w,
        );
        let ringed = !matches!(self.part, DialpadPart::DialLeft | DialpadPart::DialRight);
        let view_hover = self.view.clone();
        let view_click = self.view;

        div()
            .id(("dialpad-photo-part", idx))
            .role(Role::Button)
            .aria_label(a11y_label)
            .debug_selector(move || format!("dialpad-part-{idx}"))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .absolute()
            .left(px(rect.left))
            .top(px(rect.top))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded_tl(tl)
            .rounded_tr(tr)
            .rounded_br(br)
            .rounded_bl(bl)
            .when(ringed, |part| {
                part.border(px(PHOTO_RING_W)).border_color(ring).when(
                    state == KeyState::Selected,
                    |part| {
                        part.border(px(PHOTO_SELECTED_RING_W))
                            .child(super::selected_picture_halo(radii, PHOTO_SELECTED_RING_W))
                    },
                )
            })
            .cursor_pointer()
            .child(content)
            .on_hover(move |hovered, _window, cx| {
                let next = (*hovered).then_some(idx);
                view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
            })
            .on_click(move |_ev, _window, cx| {
                view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
            })
    }
}

/// What a part on the photo shows: a dial half its turning glyph and binding,
/// inset toward the dial's centre clear of its curved rim; a key or roller
/// half only a badge with its bound action's icon, if it has one.
fn photo_part_content(
    part: DialpadPart,
    (binding, binding_icon): (SharedString, Option<&'static str>),
    state: KeyState,
    w: f32,
) -> gpui::AnyElement {
    let accent_or = |rest: Hsla| {
        if state.is_highlighted() {
            rgb(ACCENT_BLUE).into()
        } else {
            rest
        }
    };
    match part {
        DialpadPart::DialLeft | DialpadPart::DialRight => {
            let glyph = match part.glyph() {
                PartGlyph::Icon(path) => Some(path),
                PartGlyph::RoundKey => None,
            };
            // Inset toward the dial's centre, clear of its curved rim.
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .map(|half| match part {
                    DialpadPart::DialLeft => half.pl(px(w * 0.18)),
                    _ => half.pr(px(w * 0.18)),
                })
                .children(glyph.map(|path| {
                    svg()
                        .path(path)
                        .size(px(22.))
                        .text_color(accent_or(device_highlight(0.85)))
                }))
                .child(
                    div()
                        .max_w(px(w * 0.62))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_caption()
                        .text_color(accent_or(device_highlight(0.78)))
                        .child(binding),
                )
                .into_any_element()
        }
        DialpadPart::Key(_) | DialpadPart::RollerUp | DialpadPart::RollerDown => h_flex()
            .size_full()
            .items_center()
            .justify_center()
            .children(binding_icon.map(|icon| {
                h_flex()
                    .size(px(BADGE))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(device_scrim(0.62))
                    .child(
                        svg()
                            .path(icon)
                            .size(px(BADGE_ICON))
                            .text_color(accent_or(device_highlight(0.9))),
                    )
            }))
            .into_any_element(),
    }
}

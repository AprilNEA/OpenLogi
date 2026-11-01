//! The keypad's surfaces in the Keys view: its binding-page bar, the drawn
//! keycaps beside a markerless render, and the keys mapped onto a render
//! whose positions are known — each grid key wearing the face the agent
//! paints on its LCD.
//!
//! Selection, hover, and the config panel are the parent view's; these only
//! draw the keypad and report clicks and hovers back to it.

use gpui::{
    App, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, hsla,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariant, ButtonVariants as _};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    Disableable as _, Icon, IconName, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use openlogi_core::binding::{Cid, GridKey, KeypadPage};

use super::key_points::{
    KEYCAP_H, KEYCAP_PAGING_H, KEYCAP_W, KeyShape, KeyTarget, is_paging_cid, keypad_canvas,
    photo_key_shape,
};
use super::{
    FALLBACK_KEYBOARD_SIZE, FunctionRowView, KEYBOARD_MIN_IMG_H, KEYBOARD_W, KEYS_VERTICAL_RESERVE,
    KeySlot, binding_line, image_or_fallback, key_is_highlighted,
};
use crate::features::mouse::geometry::asset_dimensions_for_png;
use crate::services::assets::ResolvedAsset;
use crate::state::AppState;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

/// A keypad's last binding page — the one the page bar can remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LastPage {
    /// The page.
    pub(super) page: KeypadPage,
    /// Whether any profile binds a key on it, so removing it loses bindings.
    pub(super) binds_keys: bool,
}

/// A keypad's binding pages: pick the page the grid keys are edited on, add
/// one after the last, or remove the last. Removing a page that still binds
/// keys asks first, because its bindings go with it; an empty one just goes.
pub(super) fn keypad_page_bar(
    page: KeypadPage,
    last_page: LastPage,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> impl IntoElement {
    let last = last_page.page;
    let pages: Vec<KeypadPage> = last.through().collect();
    let select = {
        let view = view.clone();
        let pages = pages.clone();
        move |indices: &Vec<usize>, _: &mut Window, cx: &mut App| {
            if let Some(page) = indices.first().and_then(|index| pages.get(*index)) {
                view.update(cx, |view, cx| view.set_keypad_page(*page, cx));
            }
        }
    };
    let group = ButtonGroup::new("keypad-pages")
        .outline()
        .small()
        .children(pages.iter().map(|candidate| {
            Button::new(("keypad-page", usize::from(candidate.number())))
                .label(candidate.to_string())
                .selected(*candidate == page)
        }))
        .on_click(select);
    let added = KeypadPage::new(last.number().saturating_add(1));
    let add = {
        let view = view.clone();
        Button::new("keypad-add-page")
            .ghost()
            .small()
            .icon(IconName::Plus)
            .label(tr!("keyboard.add_page"))
            .disabled(added.is_none())
            .on_click(move |_, _, cx| {
                let Some(added) = added else { return };
                AppState::apply(cx, |state| state.commit_keypad_pages(added));
                view.update(cx, |view, cx| view.set_keypad_page(added, cx));
            })
    };
    let remaining = KeypadPage::new(last.number().saturating_sub(1));
    let remove = Button::new("keypad-remove-page")
        .ghost()
        .small()
        .label(tr!("keyboard.remove_page", page => last.number()))
        .disabled(remaining.is_none())
        .on_click(move |_, window, cx| {
            let Some(remaining) = remaining else { return };
            if !last_page.binds_keys {
                AppState::apply(cx, |state| state.commit_keypad_pages(remaining));
                return;
            }
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title(tr!("keyboard.remove_page_question", page => last.number()))
                    .description(tr!("keyboard.remove_page_description"))
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text(tr!("keyboard.remove_page", page => last.number()))
                            .ok_variant(ButtonVariant::Danger)
                            .cancel_text(tr!("common.cancel"))
                            .show_cancel(true),
                    )
                    .on_ok(move |_, _, cx| {
                        AppState::apply(cx, |state| state.commit_keypad_pages(remaining));
                        true
                    })
            });
        });
    h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("keyboard.pages")),
        )
        .child(group)
        .child(add)
        .child(remove)
}

/// A markerless keypad: the device photo beside keycaps drawn from the
/// device model — the nine LCD keys as the 3×3 array they physically are and
/// the two paging keys as lower caps beneath the first two columns. The
/// photo establishes identity; the keycaps are the click targets, so no
/// marker coordinates are invented and nothing pretends the device has keys
/// it lacks. Keycaps carry their binding like callout bubbles, open the same
/// panel on click, and share the selection and hover treatments.
#[derive(IntoElement)]
pub(super) struct KeypadPane {
    slots: Vec<KeySlot>,
    image: Option<std::path::PathBuf>,
    photo: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl KeypadPane {
    pub(super) fn new(
        slots: Vec<KeySlot>,
        image: Option<std::path::PathBuf>,
        photo: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
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

impl RenderOnce for KeypadPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let canvas = keypad_canvas();
        let (selected, hovered) = (self.selected, self.hovered);
        let view = self.view;
        let pal = theme::palette(cx);
        let (photo_w, photo_h) = self.photo;
        let keycaps =
            div()
                .relative()
                .w(px(canvas.0))
                .h(px(canvas.1))
                .children(self.slots.into_iter().map(move |slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    KeyCap {
                        cap: (KEYCAP_W, keycap_height(&slot.target)),
                        slot,
                        highlighted,
                        canvas,
                        view: view.clone(),
                    }
                }));
        // The render keeps the device's identity; the keycaps, sized to the
        // same height, form the aligned pair that replaces photo-plus-band.
        h_flex()
            .items_center()
            .gap_6()
            .when_some(self.image, |row, path| {
                row.child(image_or_fallback(Some(path), photo_w, photo_h, &pal))
            })
            .child(keycaps)
    }
}

/// A keypad drawn the way the device looks: its render with each key mapped
/// onto the physical key the depot's markers (or the stock render's measured
/// centres) name. An LCD
/// grid key shows the face the agent paints on the device — the bound
/// action's glyph, white on black, or nothing when the key does nothing —
/// and the paging keys stay the printed keys of the render, outlined when
/// hovered or selected. Every key is its own click target, opening the same
/// panel as any other key.
#[derive(IntoElement)]
pub(super) struct KeypadPhotoPane {
    slots: Vec<KeySlot>,
    image: Option<std::path::PathBuf>,
    render_size: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl KeypadPhotoPane {
    pub(super) fn new(
        slots: Vec<KeySlot>,
        image: Option<std::path::PathBuf>,
        render_size: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
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

impl RenderOnce for KeypadPhotoPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (img_w, img_h) = self.render_size;
        let (selected, hovered) = (self.selected, self.hovered);
        let view = self.view;
        let pal = theme::palette(cx);

        div()
            .relative()
            .w(px(img_w))
            .h(px(img_h))
            .child(image_or_fallback(self.image, img_w, img_h, &pal))
            .children(self.slots.into_iter().map(move |slot| PhotoKey {
                state: KeyState::of(slot.idx, selected, hovered),
                shape: photo_key_shape(&slot.target, img_w),
                render: (img_w, img_h),
                slot,
                view: view.clone(),
            }))
    }
}

/// Share of the face the glyph spans when the face also carries a caption.
const LCD_GLYPH_FRAC: f32 = 0.38;
/// Gap between a key's edge and its selection ring, in px.
const PHOTO_RING_GAP: f32 = 3.;
const PHOTO_RING_W: f32 = 2.;

/// How a key on the render is drawn right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyState {
    Rest,
    Hovered,
    Selected,
}

impl KeyState {
    fn of(idx: usize, selected: Option<usize>, hovered: Option<usize>) -> Self {
        if selected == Some(idx) {
            Self::Selected
        } else if hovered == Some(idx) {
            Self::Hovered
        } else {
            Self::Rest
        }
    }
}

/// One key mapped onto the keypad render: a selection ring around the
/// physical key and, for an LCD grid key, the face the device shows.
#[derive(IntoElement)]
struct PhotoKey {
    slot: KeySlot,
    state: KeyState,
    /// The key's outline on the render, in px.
    shape: KeyShape,
    /// The render's size, in px — what the slot's marker fractions scale by.
    render: (f32, f32),
    view: Entity<FunctionRowView>,
}

impl RenderOnce for PhotoKey {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let shape = self.shape;
        let idx = self.slot.idx;
        let state = self.state;
        let outer = PHOTO_RING_GAP + PHOTO_RING_W;
        let left = self.slot.x_frac * self.render.0 - shape.w / 2. - outer;
        let top = self.slot.y_frac * self.render.1 - shape.h / 2. - outer;
        let tooltip: SharedString = format!("{} · {}", self.slot.label, self.slot.binding).into();
        let ring = match state {
            KeyState::Selected => rgb(ACCENT_BLUE).into(),
            KeyState::Hovered => hsla(0., 0., 1., 0.55),
            KeyState::Rest => hsla(0., 0., 0., 0.),
        };
        let face = matches!(self.slot.target, KeyTarget::Grid(_))
            .then(|| lcd_face(self.slot.lcd_icon, self.slot.binding.clone(), shape, state));
        // The ring follows the key's own curve, one gap further out.
        let [tl, tr, br, bl] = shape.radii.map(|r| px(r + outer));
        let view_hover = self.view.clone();
        let view_click = self.view;

        div()
            .id(("keypad-photo-key", idx))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(shape.w + 2. * outer))
            .h(px(shape.h + 2. * outer))
            .p(px(PHOTO_RING_GAP))
            .rounded_tl(tl)
            .rounded_tr(tr)
            .rounded_br(br)
            .rounded_bl(bl)
            .border(px(PHOTO_RING_W))
            .border_color(ring)
            .cursor_pointer()
            .children(face)
            .on_hover(move |hovered, _window, cx| {
                let next = (*hovered).then_some(idx);
                view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
            })
            .on_click(move |_ev, _window, cx| {
                view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
            })
    }
}

/// An LCD grid key's face as the device shows it: the key's whole top in
/// black, carrying the bound action's glyph in white (the same icon the agent
/// writes to the key) with the action's name beneath, or blank when the key
/// does nothing — a blank key offers a faint plus while it is hovered or
/// selected.
fn lcd_face(
    icon: Option<&'static str>,
    binding: SharedString,
    key: KeyShape,
    state: KeyState,
) -> gpui::Div {
    let glyph = key.w * LCD_GLYPH_FRAC;
    let [tl, tr, br, bl] = key.radii.map(px);
    let face = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(key.w * 0.06))
        .rounded_tl(tl)
        .rounded_tr(tr)
        .rounded_br(br)
        .rounded_bl(bl)
        .bg(hsla(0., 0., 0., 1.))
        .overflow_hidden();
    match icon {
        Some(icon) => face
            .child(
                svg()
                    .path(icon)
                    .size(px(glyph))
                    .flex_none()
                    .text_color(hsla(0., 0., 1., 1.)),
            )
            .child(
                div()
                    .max_w(px(key.w - 16.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_caption()
                    .text_color(hsla(0., 0., 1., 0.72))
                    .child(binding),
            ),
        None => face.when(state != KeyState::Rest, |face| {
            face.child(
                Icon::new(IconName::Plus)
                    .size(px(glyph * 0.6))
                    .text_color(hsla(0., 0., 1., 0.45)),
            )
        }),
    }
}

/// The overlay render size: the portrait keypad render as tall as the
/// viewport allows under the same reserve the keyboard path keeps, minus
/// the callout band this pane never draws, capped at [`KEYBOARD_W`].
pub(super) fn keypad_photo_render_size(
    asset: Option<&ResolvedAsset>,
    viewport_h: f32,
) -> (f32, f32) {
    let Some(asset) = asset.filter(|asset| asset.png_height > 0) else {
        return FALLBACK_KEYBOARD_SIZE;
    };
    let target_h = (viewport_h - KEYS_VERTICAL_RESERVE).max(KEYBOARD_MIN_IMG_H);
    asset_dimensions_for_png(asset, target_h, KEYBOARD_W)
}

/// One keycap: face, binding line, click behavior. The keycap itself is the
/// click target — no bubble, no leader line — and it wears the same
/// selected/hover treatment as the photo path's callout bubbles. `canvas`
/// is the space `slot.point` positions it in (the keycap canvas or the
/// render); `cap` is its size in that space.
#[derive(IntoElement)]
struct KeyCap {
    slot: KeySlot,
    highlighted: bool,
    canvas: (f32, f32),
    cap: (f32, f32),
    view: Entity<FunctionRowView>,
}

impl RenderOnce for KeyCap {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (w, h) = self.canvas;
        let (cap_w, cap_h) = self.cap;
        let pal = theme::palette(cx);
        let idx = self.slot.idx;
        let glyph = keycap_glyph(&self.slot.target);
        let highlighted = self.highlighted;
        let tooltip = self.slot.label.clone();
        let legend = self.slot.label;
        let binding = self.slot.binding;
        let binding_icon = self.slot.binding_icon;
        let left = self.slot.x_frac * w - cap_w / 2.;
        let top = self.slot.y_frac * h - cap_h / 2.;
        let view_hover = self.view.clone();
        let view_click = self.view;

        div()
            .id(("keycap", idx))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(cap_w))
            .h(px(cap_h))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(1.))
            .rounded_md()
            .border_1()
            .border_color(if highlighted {
                rgb(ACCENT_BLUE).into()
            } else {
                pal.border
            })
            .bg(if highlighted {
                theme::accent_tint()
            } else {
                pal.control
            })
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if highlighted {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .child(
                div()
                    .max_w(px(cap_w - 8.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if highlighted {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_primary
                    })
                    .child(match glyph {
                        Some(glyph) => glyph.to_string(),
                        None => legend.to_string(),
                    }),
            )
            .child(binding_line(
                binding,
                binding_icon,
                cap_w - 8.,
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

/// The keycap's face: height and glyph. The schematic canvas sizes grid caps
/// by that model; the photo overlay sizes its caps by the depot's markers.
/// Controls outside the keypad tables draw their localized name, never a
/// made-up glyph.
fn keycap_height(target: &KeyTarget) -> f32 {
    match target {
        KeyTarget::Control(button) if button.cid().is_some_and(is_paging_cid) => KEYCAP_PAGING_H,
        KeyTarget::Control(_) | KeyTarget::Grid(_) | KeyTarget::FunctionKey(_) => KEYCAP_H,
    }
}

/// The glyph drawn on a cap's face.
fn keycap_glyph(target: &KeyTarget) -> Option<&'static str> {
    match target {
        KeyTarget::Grid(slot) => Some(grid_glyph(slot.key)),
        KeyTarget::Control(button) => button.cid().and_then(paging_glyph),
        KeyTarget::FunctionKey(_) => None,
    }
}

/// Position glyphs mirroring the physical keypad. Locales differ, geometries
/// don't — and the full key name stays one tooltip away.
fn grid_glyph(key: GridKey) -> &'static str {
    match key {
        GridKey::TopLeft => "↖",
        GridKey::TopMiddle => "↑",
        GridKey::TopRight => "↗",
        GridKey::MiddleLeft => "←",
        GridKey::Middle => "●",
        GridKey::MiddleRight => "→",
        GridKey::BottomLeft => "↙",
        GridKey::BottomMiddle => "↓",
        GridKey::BottomRight => "↘",
    }
}

/// The two paging keys' printed legends.
fn paging_glyph(cid: Cid) -> Option<&'static str> {
    match cid {
        cid if cid == Cid::new(0x01a1) => Some("‹"),
        cid if cid == Cid::new(0x01a2) => Some("›"),
        _ => None,
    }
}

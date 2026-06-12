//! Popover content for binding mouse buttons, plus the gesture button's custom
//! two-level menu.
//!
//! - [`action_picker`] — one button → one [`Action`], rendered as a custom flat
//!   list inside a gpui-component [`Popover`](gpui_component::popover::Popover).
//!   Generic over the entity that should be notified after a binding changes so
//!   the trigger re-renders with the new label.
//! - [`gesture_overview`] — the gesture button's custom multi-level menu: a
//!   plus-shaped navigator card (level 1) listing all five [`GestureDirection`]s
//!   with their bound actions, and — once a direction is activated — a separate
//!   action-list card (level 2) that flies out beside it. The two are distinct
//!   floating cards (own surface + height), so this reads like a cascading menu
//!   while staying fully custom-styled. The active direction is scratch state on
//!   the [`MouseModelView`].
//!
//! The [`action_picker`] [`Popover`] uses the framework's styled surface; the
//! gesture menu uses `appearance(false)` and draws its own card surfaces, since
//! its two levels need independent panels. Rows are transparent until hovered;
//! the active binding is marked with accent text plus a check glyph.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, BorrowAppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement as _, Styled, Window, div, hsla,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Icon, IconName, h_flex, popover::PopoverState, v_flex};

use crate::data::mouse_buttons::{
    Action, ButtonId, Category, GestureDirection, default_gesture_binding,
};
use crate::mouse_model::view::MouseModelView;
use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette};

/// Floor width for the [`action_picker`] popover. The action labels drive the
/// actual width; this only stops the list from collapsing too narrow. Matches
/// gpui-component's own `PopupMenu` floor (`min_w(rems(8.))`).
const POPOVER_W: f32 = 128.;

/// Cap the scrollable action list height. The catalog has 29+ entries across
/// half a dozen categories; without a cap the list overflows the window.
const POPOVER_LIST_MAX_H: f32 = 360.;

/// Build the popover body that re-binds a single `btn`.
///
/// `observer` is whatever entity wraps the trigger — it's notified after the
/// global updates so the trigger re-renders. Picking an action commits it and
/// dismisses the popover.
pub fn action_picker<T: 'static>(
    btn: ButtonId,
    observer: &Entity<T>,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let current = cx
        .try_global::<AppState>()
        .and_then(|s| s.button_bindings.get(&btn).cloned());

    let observer = observer.clone();
    let popover = cx.entity().downgrade();
    let on_pick: PickFn = Rc::new(move |action, window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_binding(btn, action));
        observer.update(cx, |_, cx| cx.notify());
        if let Some(p) = popover.upgrade() {
            p.update(cx, |s, cx| s.dismiss(window, cx));
        }
    });

    let pal = theme::palette(cx);
    let button = rust_i18n::t!(btn.label());
    menu_card(pal)
        .min_w(px(POPOVER_W))
        .child(title(tr!("Bind %{name}", name => button), pal))
        .child(divider(pal))
        .child(scroll_list(
            "picker-scroll",
            action_rows("action-item", current.as_ref(), &on_pick, pal),
        ))
        .into_any_element()
}

/// Floor width of a single direction cell in the plus navigator. Three sit side
/// by side in the middle row, so the plus is roughly `3×` this plus gaps.
const GESTURE_CELL_W: f32 = 104.;

/// Build the gesture button's custom two-level menu: the plus navigator card
/// (level 1) plus, once a direction is activated, its action-list card (level 2)
/// flown out beside it. The two are separate floating cards — own surface and
/// height — so it reads like a cascading menu without sharing one box. The
/// active direction is scratch UI state on the [`MouseModelView`] (`None` until
/// a cell is clicked → only the plus shows), reset on popover close. Mutating it
/// re-renders the view, which re-renders this open popover's content.
pub fn gesture_overview(
    view: &Entity<MouseModelView>,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let pal = theme::palette(cx);
    let active = view.read(cx).gesture_selected_dir();
    h_flex()
        .items_start()
        .gap_2()
        .child(plus_card(view, active, pal, cx))
        // The flyout card only appears once a direction is activated.
        .when_some(active, |row, dir| row.child(flyout_card(dir, view, pal, cx)))
        .into_any_element()
}

/// The shared floating-card surface for every binding menu — the button picker,
/// the gesture plus navigator, and its action flyout — so they read as one
/// consistent, app-branded panel instead of two different surfaces.
///
/// Radius scale (shape lock): interactive rows/cells use `rounded_md` (6px); the
/// card uses `rounded_lg` (8px). The shadow is gpui's soft `shadow_md`, not a
/// hard drop. Not stateful (no interaction → no element id, so two sibling cards
/// can't collide on one).
fn menu_card(pal: Palette) -> gpui::Div {
    v_flex()
        .bg(pal.surface)
        .border_1()
        .border_color(pal.border)
        .rounded_lg()
        .shadow_md()
        .p_1p5()
}

/// Level 1: the plus navigator. `Up` on top, `Left`/`Click`/`Right` across the
/// middle, `Down` on the bottom. Each cell shows its glyph + label and bound
/// action; the `active` cell (if any) is accented. Clicking a cell activates
/// that direction (flying out the level-2 card) without committing.
fn plus_card(
    view: &Entity<MouseModelView>,
    active: Option<GestureDirection>,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let actions: BTreeMap<GestureDirection, Action> = GestureDirection::ALL
        .into_iter()
        .map(|d| {
            let action = cx
                .try_global::<AppState>()
                .and_then(|s| s.gesture_bindings.get(&d).cloned())
                .unwrap_or_else(|| default_gesture_binding(d));
            (d, action)
        })
        .collect();

    let cell =
        |dir: GestureDirection| direction_cell(dir, &actions[&dir], active == Some(dir), view, pal);

    menu_card(pal)
        .gap_1p5()
        .child(
            h_flex()
                .w_full()
                .justify_center()
                .child(cell(GestureDirection::Up)),
        )
        .child(
            h_flex()
                .w_full()
                .justify_center()
                .gap_1p5()
                .child(cell(GestureDirection::Left))
                .child(cell(GestureDirection::Click))
                .child(cell(GestureDirection::Right)),
        )
        .child(
            h_flex()
                .w_full()
                .justify_center()
                .child(cell(GestureDirection::Down)),
        )
        .into_any_element()
}

/// One direction's cell in the plus: a fixed-width clickable card with the
/// direction glyph + label above its bound-action label. The `active` cell is
/// accented (border + faint fill); a default binding's action is muted.
fn direction_cell(
    dir: GestureDirection,
    current: &Action,
    active: bool,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let idx = match dir {
        GestureDirection::Up => 0usize,
        GestureDirection::Down => 1,
        GestureDirection::Left => 2,
        GestureDirection::Right => 3,
        GestureDirection::Click => 4,
    };
    let header = format!("{}  {}", dir.glyph(), tr!(dir.label()));
    let action_label = tr!(current.label());
    let is_default = *current == default_gesture_binding(dir);
    let view = view.clone();
    v_flex()
        .id(("gesture-cell", idx))
        .w(px(GESTURE_CELL_W))
        .gap(px(2.))
        .px_2()
        .py_1p5()
        .rounded_md()
        .border_1()
        .border_color(if active {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .when(active, |s| s.bg(hsla(0.6, 0.9, 0.6, 0.10)))
        .hover(move |s| s.bg(pal.surface_hover))
        .child(div().text_xs().text_color(pal.text_muted).child(header))
        .child(
            div()
                .text_sm()
                .text_color(if is_default {
                    pal.text_muted
                } else {
                    pal.text_primary
                })
                .child(action_label),
        )
        // Click opens this direction's flyout; clicking the active cell again
        // closes it. (Hover-to-open was too easy to mis-trigger while moving the
        // cursor across the plus.)
        .on_click(move |_event, _window, cx| {
            view.update(cx, |v, vcx| {
                let next = (v.gesture_selected_dir() != Some(dir)).then_some(dir);
                v.set_gesture_selected_dir(next);
                vcx.notify();
            });
        })
        .into_any_element()
}

/// Level 2: the `dir` direction's action picker, flown out as its own card —
/// the category-grouped catalog with the current binding checked. Picking
/// commits and stays open, so the level-1 cell + checkmark update in place and
/// the user can keep editing other directions.
fn flyout_card(
    dir: GestureDirection,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let current = cx
        .try_global::<AppState>()
        .and_then(|s| s.gesture_bindings.get(&dir).cloned())
        .unwrap_or_else(|| default_gesture_binding(dir));

    let view_pick = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_gesture_binding(dir, action));
        // Stay open; re-render so the level-1 cell + checkmark update.
        view_pick.update(cx, |_, vcx| vcx.notify());
    });

    menu_card(pal)
        .min_w(px(POPOVER_W))
        .child(title(format!("{}  {}", dir.glyph(), tr!(dir.label())), pal))
        .child(divider(pal))
        .child(scroll_list(
            "gesture-dir-scroll",
            action_rows("gesture-action", Some(&current), &on_pick, pal),
        ))
        .into_any_element()
}

// ── Shared building blocks ──────────────────────────────────────────────────

/// Commit callback invoked when a row is clicked. Boxed so the row builder can
/// be shared between the button picker and any future custom picker, which
/// differ only in what they do after committing.
type PickFn = Rc<dyn Fn(Action, &mut Window, &mut App)>;

/// The action catalog grouped by [`Category`], preserving catalog order within
/// each group and first-seen order across groups.
fn grouped_catalog() -> Vec<(Category, Vec<Action>)> {
    let mut sections: Vec<(Category, Vec<Action>)> = Vec::new();
    for action in Action::catalog() {
        let cat = action.category();
        if let Some(sec) = sections.iter_mut().find(|(c, _)| *c == cat) {
            sec.1.push(action);
        } else {
            sections.push((cat, vec![action]));
        }
    }
    sections
}

/// Icon for the gesture button's label card — lucide `move` (a 4-way arrow
/// cross), standing in for its five swipe directions since it has no single
/// bound action.
pub(crate) const GESTURE_BUTTON_ICON: &str = "action-icons/move.svg";

/// Asset path (served by [`crate::app_assets`]) of the vendored lucide glyph for
/// an action — the leading icon in each action row and in the leader-line label
/// card. Exhaustive on purpose: a new [`Action`] variant must pick an icon here
/// (no catch-all fallback).
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    match action {
        Action::None => "action-icons/ban.svg",
        Action::LeftClick | Action::RightClick => "action-icons/mouse-pointer-click.svg",
        Action::MiddleClick => "action-icons/mouse.svg",
        // Circled arrows: visually "back/forward as a button", distinct from
        // BrowserBack/BrowserForward's bare arrows in the Navigation section.
        Action::MouseBack => "action-icons/circle-arrow-left.svg",
        Action::MouseForward => "action-icons/circle-arrow-right.svg",
        Action::Copy => "action-icons/copy.svg",
        Action::Paste => "action-icons/clipboard-paste.svg",
        Action::Cut => "action-icons/scissors.svg",
        Action::Undo => "action-icons/undo-2.svg",
        Action::Redo => "action-icons/redo-2.svg",
        Action::SelectAll => "action-icons/list-checks.svg",
        Action::Find => "action-icons/search.svg",
        Action::Save => "action-icons/save.svg",
        Action::BrowserBack => "action-icons/arrow-left.svg",
        Action::BrowserForward => "action-icons/arrow-right.svg",
        Action::NewTab => "action-icons/square-plus.svg",
        Action::CloseTab => "action-icons/square-x.svg",
        Action::ReopenTab => "action-icons/rotate-ccw.svg",
        Action::NextTab => "action-icons/chevron-right.svg",
        Action::PrevTab => "action-icons/chevron-left.svg",
        Action::ReloadPage => "action-icons/rotate-cw.svg",
        Action::MissionControl => "action-icons/layout-grid.svg",
        Action::AppExpose => "action-icons/layers.svg",
        Action::PreviousDesktop => "action-icons/square-arrow-left.svg",
        Action::NextDesktop => "action-icons/square-arrow-right.svg",
        Action::ShowDesktop => "action-icons/monitor.svg",
        Action::LaunchpadShow => "action-icons/grid-3x3.svg",
        Action::LockScreen => "action-icons/lock.svg",
        Action::Screenshot => "action-icons/camera.svg",
        Action::PlayPause => "action-icons/play.svg",
        Action::NextTrack => "action-icons/skip-forward.svg",
        Action::PrevTrack => "action-icons/skip-back.svg",
        Action::VolumeUp => "action-icons/volume-2.svg",
        Action::VolumeDown => "action-icons/volume-1.svg",
        Action::MuteVolume => "action-icons/volume-x.svg",
        Action::CycleDpiPresets | Action::SetDpiPreset(_) => "action-icons/gauge.svg",
        Action::ToggleSmartShift => "action-icons/refresh-cw.svg",
        Action::ScrollUp => "action-icons/chevrons-up.svg",
        Action::ScrollDown => "action-icons/chevrons-down.svg",
        Action::HorizontalScrollLeft => "action-icons/chevrons-left.svg",
        Action::HorizontalScrollRight => "action-icons/chevrons-right.svg",
        Action::CustomShortcut(_) => "action-icons/keyboard.svg",
    }
}

/// Build the category-grouped action rows. Each row leads with the action's
/// icon, then its label; `current` adds a trailing accent check. Clicking any
/// row invokes `on_pick`. `id_prefix` disambiguates element IDs between pickers
/// that share this builder.
fn action_rows(
    id_prefix: &'static str,
    current: Option<&Action>,
    on_pick: &PickFn,
    pal: Palette,
) -> Vec<AnyElement> {
    let mut idx = 0usize;
    let mut children: Vec<AnyElement> = Vec::new();
    for (category, actions) in grouped_catalog() {
        let category_label = rust_i18n::t!(category.label());
        children.push(section_header(&category_label, pal));
        for action in actions {
            let selected = current == Some(&action);
            let label = tr!(action.label());
            let icon_path = action_icon_path(&action);
            let on_pick = on_pick.clone();
            let row_id = idx;
            idx += 1;
            children.push(
                menu_row((id_prefix, row_id), pal, selected)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                svg()
                                    .path(icon_path)
                                    .size_4()
                                    .flex_none()
                                    .text_color(pal.text_muted),
                            )
                            .child(div().child(label)),
                    )
                    .when(selected, |s| {
                        s.child(
                            Icon::new(IconName::Check)
                                .size_3()
                                .text_color(rgb(ACCENT_BLUE)),
                        )
                    })
                    .on_click(move |_event, window, cx| (on_pick)(action.clone(), window, cx))
                    .into_any_element(),
            );
        }
    }
    children
}

/// A clickable, full-width menu row: `text-sm`, children spread left/right.
/// The label stays in `text_primary` in both states for readability; selection
/// is shown by a subtle accent fill (plus the caller's trailing check), and the
/// fill deepens on hover. Unselected rows are transparent at rest, neutral on
/// hover. One accent, one signal per state — no blue label text (which fails AA
/// contrast on the near-white surface).
fn menu_row(
    id: impl Into<gpui::ElementId>,
    pal: Palette,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    // Accent fill derived from ACCENT_BLUE (≈ hue 0.6 / sat 0.9 / light 0.6),
    // kept low-alpha so the row reads as tinted, not painted.
    let tint = hsla(0.6, 0.9, 0.6, 0.12);
    let tint_hover = hsla(0.6, 0.9, 0.6, 0.18);
    h_flex()
        .id(id)
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .text_sm()
        .text_color(pal.text_primary)
        .when(selected, |s| s.bg(tint))
        .hover(move |s| {
            s.bg(if selected {
                tint_hover
            } else {
                pal.surface_hover
            })
        })
}

/// Small uppercase muted group header.
fn section_header(label: &str, pal: Palette) -> AnyElement {
    div()
        .w_full()
        .px_2()
        .pt_2()
        .pb_0p5()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(pal.text_muted)
        .child(label.to_uppercase())
        .into_any_element()
}

/// Popover title — the binding context, e.g. "Bind Back".
fn title(text: impl Into<gpui::SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .px_2()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(pal.text_muted)
        .child(text.into())
}

/// 1px hairline separating the title from the list.
fn divider(pal: Palette) -> impl IntoElement {
    div().mb_1().h(px(1.)).w_full().bg(pal.border)
}

/// Wrap `rows` in the height-capped, vertically scrollable list region.
fn scroll_list(id: &'static str, rows: Vec<AnyElement>) -> impl IntoElement {
    div()
        .id(id)
        .max_h(px(POPOVER_LIST_MAX_H))
        .overflow_y_scroll()
        .children(rows)
}

//! Popover content for binding mouse buttons, plus the gesture button's
//! cascading menu.
//!
//! - [`action_picker`] — one button → one [`Action`], rendered as a custom flat
//!   list inside a gpui-component [`Popover`](gpui_component::popover::Popover).
//!   Generic over the entity that should be notified after a binding changes so
//!   the trigger re-renders with the new label.
//! - [`build_gesture_menu`] — the gesture button's two-level
//!   [`PopupMenu`](gpui_component::menu::PopupMenu): one submenu per
//!   [`GestureDirection`], each listing the full action catalog with the
//!   current binding checked. Picking an action commits straight to
//!   [`AppState`], whose global observers re-render the model, so the menu
//!   needs no observer.
//!
//! The [`Popover`] wraps the [`action_picker`] content in a styled surface
//! (background, border, shadow, `p_3` padding), so the layout here stays flat:
//! no extra card background, no extra outer padding. Rows are transparent until
//! hovered; the active binding is marked with accent text plus a check glyph
//! rather than a filled box. The [`PopupMenu`] draws its own surface and check
//! marks, so the gesture menu defers entirely to the framework's styling.

use std::rc::Rc;

use gpui::{
    AnyElement, App, BorrowAppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement as _, Styled, Window, div, hsla,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
    popover::PopoverState,
    v_flex,
};

use crate::data::mouse_buttons::{
    Action, ButtonId, Category, GestureDirection, default_gesture_binding, localized_action_label,
};
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
    v_flex()
        .min_w(px(POPOVER_W))
        .child(title(tr!("Bind %{name}", name => button), pal))
        .child(divider(pal))
        .child(scroll_list(
            "picker-scroll",
            action_rows("action-item", current.as_ref(), &on_pick, pal),
        ))
        .into_any_element()
}

/// Build the gesture button's two-level [`PopupMenu`]: one submenu per
/// [`GestureDirection`], each opening the full action catalog with the current
/// binding checked. Picking an action commits it to [`AppState`] and dismisses
/// the whole menu.
pub fn build_gesture_menu(
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for dir in GestureDirection::ALL {
        // Glyph prefix gives the directional cue the old list had; the bound
        // action shows up as the checked row inside each submenu.
        let label = format!("{}  {}", dir.glyph(), tr!(dir.label()));
        menu = menu.submenu(label, window, cx, move |submenu, _window, cx| {
            gesture_action_submenu(dir, submenu, cx)
        });
    }
    menu
}

/// Populate one direction's action submenu: the category-grouped catalog with
/// the current binding checked and a commit-on-click handler per row. The
/// submenu scrolls (the catalog is taller than the window) — it has no further
/// submenus of its own, so scrolling is allowed.
fn gesture_action_submenu(
    direction: GestureDirection,
    submenu: PopupMenu,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let current = cx
        .try_global::<AppState>()
        .and_then(|s| s.gesture_bindings.get(&direction).cloned())
        .unwrap_or_else(|| default_gesture_binding(direction));

    let mut submenu = submenu.scrollable(true).max_h(px(POPOVER_LIST_MAX_H));
    for (category, actions) in grouped_catalog() {
        submenu = submenu.label(tr!(category.label()));
        for action in actions {
            let checked = action == current;
            let label = localized_action_label(&action);
            let commit = action;
            submenu = submenu.item(PopupMenuItem::new(label).checked(checked).on_click(
                move |_event, _window, cx| {
                    let action = commit.clone();
                    cx.update_global::<AppState, _>(move |state, _| {
                        state.commit_gesture_binding(direction, action);
                    });
                },
            ));
        }
    }
    submenu
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

/// Build the category-grouped action rows. `current` is marked with accent
/// text + a check glyph; clicking any row invokes `on_pick`. `id_prefix`
/// disambiguates element IDs between pickers that share this builder.
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
            let label = localized_action_label(&action);
            let on_pick = on_pick.clone();
            let row_id = idx;
            idx += 1;
            children.push(
                menu_row((id_prefix, row_id), pal)
                    .text_color(if selected {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_primary
                    })
                    .when(selected, |s| s.bg(hsla(0.6, 0.9, 0.6, 0.14)))
                    .child(div().child(label))
                    .when(selected, |s| {
                        s.child(div().text_color(rgb(ACCENT_BLUE)).child("✓"))
                    })
                    .on_click(move |_event, window, cx| (on_pick)(action.clone(), window, cx))
                    .into_any_element(),
            );
        }
    }
    children
}

/// A clickable, full-width menu row: transparent at rest, hover-filled,
/// `text-sm`, with its children spread left/right. Children are added by the
/// caller.
fn menu_row(id: impl Into<gpui::ElementId>, pal: Palette) -> gpui::Stateful<gpui::Div> {
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
        .hover(move |s| s.bg(pal.surface_hover))
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

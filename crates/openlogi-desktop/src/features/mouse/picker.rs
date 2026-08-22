//! Popover content for binding mouse buttons, plus the per-button gesture
//! menu.
//!
//! - [`action_picker`] — one button → one [`Action`], rendered as a custom flat
//!   list inside a gpui-component [`Popover`](gpui_component::popover::Popover).
//!   Generic over the entity that should be notified after a binding changes so
//!   the trigger re-renders with the new label. Gesture-capable buttons lead
//!   with a pinned "Gestures" entry that promotes the button into gesture mode.
//! - [`gesture_overview`] — a gesture-mode button's custom multi-level menu: a
//!   plus-shaped navigator card (level 1) listing all five [`GestureDirection`]s
//!   with their bound actions, and — once a direction is activated — a separate
//!   action-list card (level 2) that flies out beside it. The two are distinct
//!   floating cards (own surface + height), so this reads like a cascading menu
//!   while staying fully custom-styled. The active direction is scratch state on
//!   the [`MouseModelView`]. A footer row demotes the button back to a single
//!   action. Any number of buttons can be in gesture mode at once, each with
//!   its own menu.
//!
//! The [`action_picker`] [`Popover`] uses the framework's styled surface; the
//! gesture menu uses `appearance(false)` and draws its own card surfaces, since
//! its two levels need independent panels. Rows are transparent until hovered;
//! the active binding is marked with accent text plus a check glyph.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, BorrowAppContext as _, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Role, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Icon, IconName, h_flex, popover::PopoverState, v_flex};
use openlogi_core::binding::{
    Action, ButtonId, Category, GestureDirection, default_binding, default_gesture_binding,
};

use super::thumbwheel::ThumbwheelPreset;
use super::view::MouseModelView;
use crate::state::AppState;
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, SelectableStyle, Typography as _};

/// Floor width for the [`action_picker`] popover. The action labels drive the
/// actual width; this only stops the list from collapsing too narrow. Matches
/// gpui-component's own `PopupMenu` floor (`min_w(rems(8.))`).
pub(crate) const POPOVER_W: f32 = 128.;

/// Cap the scrollable action list height. The catalog has 29+ entries across
/// half a dozen categories; without a cap the list overflows the window.
pub(crate) const POPOVER_LIST_MAX_H: f32 = 360.;

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
    let on_pick: PickFn = Rc::new({
        let observer = observer.clone();
        let popover = popover.clone();
        move |action, window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.commit_binding(btn, action));
            observer.update(cx, |_, cx| cx.notify());
            if let Some(p) = popover.upgrade() {
                p.update(cx, |s, cx| s.dismiss(window, cx));
            }
        }
    });

    let pal = theme::palette(cx);
    let button = rust_i18n::t!(btn.label());
    // A control that can gesture (a HID++ gesture source, or an OS-hook button
    // the hook can hold-and-swipe) leads with a pinned mode entry above the
    // action list: picking it promotes THIS button into gesture mode — any
    // number of buttons may gesture at once — and the reopened popover then
    // shows the gesture menu.
    let gesture_capable = btn.is_hidpp_gesture_source() || btn.is_os_hook_button();
    menu_card(pal)
        .min_w(px(POPOVER_W))
        .child(title(tr!("Bind %{name}", name => button), pal))
        .child(divider(pal))
        .when(gesture_capable, |card| {
            card.child(gesture_mode_row(btn, &observer, &popover, pal))
                .child(divider(pal))
        })
        .child(scroll_list(
            "picker-scroll",
            action_rows("action-item", current.as_ref(), &on_pick, pal),
        ))
        .into_any_element()
}

/// Build the paired preset picker for thumb-wheel rotation. One click updates
/// both directional bindings and dismisses the popover.
pub(crate) fn thumbwheel_picker<T: 'static>(
    observer: &Entity<T>,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let current = cx.try_global::<AppState>().and_then(|state| {
        let backward = state
            .button_bindings
            .get(&ButtonId::ThumbwheelScrollDown)
            .cloned()
            .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollDown));
        let forward = state
            .button_bindings
            .get(&ButtonId::ThumbwheelScrollUp)
            .cloned()
            .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollUp));
        ThumbwheelPreset::recognize(&backward, &forward)
    });

    let pal = theme::palette(cx);
    let popover = cx.entity().downgrade();
    let rows: Vec<AnyElement> = ThumbwheelPreset::ALL
        .into_iter()
        .enumerate()
        .map(|(idx, preset)| {
            let selected = current == Some(preset);
            let label = tr!(preset.label());
            let observer = observer.clone();
            let popover = popover.clone();
            menu_row(("thumbwheel-preset", idx), pal, selected)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path(preset.icon())
                                .size_4()
                                .flex_none()
                                .text_color(pal.text_muted),
                        )
                        .child(div().child(label)),
                )
                .when(selected, |row| {
                    row.child(
                        Icon::new(IconName::Check)
                            .size_3()
                            .text_color(rgb(ACCENT_BLUE)),
                    )
                })
                .on_click(move |_event, window, cx| {
                    cx.update_global::<AppState, _>(|state, _| {
                        state.commit_thumbwheel_preset(preset);
                    });
                    observer.update(cx, |_, cx| cx.notify());
                    if let Some(popover) = popover.upgrade() {
                        popover.update(cx, |state, cx| state.dismiss(window, cx));
                    }
                })
                .into_any_element()
        })
        .collect();

    menu_card(pal)
        .min_w(px(POPOVER_W))
        .child(title(tr!("Bind %{name}", name => tr!("Thumb Wheel")), pal))
        .child(divider(pal))
        .when(current.is_none(), |card| {
            card.child(
                div()
                    .px_2()
                    .py_1()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("Custom")),
            )
            .child(divider(pal))
        })
        .child(scroll_list("thumbwheel-picker-scroll", rows))
        .into_any_element()
}

/// The pinned "Gestures" entry leading a gesture-capable button's picker.
/// Clicking promotes the button into gesture mode (its single action becomes
/// the Click arm, swipe arms seed from defaults) and dismisses the popover;
/// reopening it lands on the gesture menu.
fn gesture_mode_row<T: 'static>(
    btn: ButtonId,
    observer: &Entity<T>,
    popover: &gpui::WeakEntity<PopoverState>,
    pal: Palette,
) -> AnyElement {
    let observer = observer.clone();
    let popover = popover.clone();
    menu_row("gesture-mode-row", pal, false)
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(GESTURE_BUTTON_ICON)
                        .size_4()
                        .flex_none()
                        .text_color(pal.text_muted),
                )
                .child(div().child(tr!("Gestures"))),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size_3()
                .text_color(pal.text_muted),
        )
        .on_click(move |_event, window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.commit_gesture_mode(btn, true));
            observer.update(cx, |_, cx| cx.notify());
            if let Some(p) = popover.upgrade() {
                p.update(cx, |s, cx| s.dismiss(window, cx));
            }
        })
        .into_any_element()
}

/// Floor width of a single direction cell in the plus navigator. Three sit side
/// by side in the middle row, so the plus is roughly `3×` this plus gaps.
const GESTURE_CELL_W: f32 = 104.;

/// Build `btn`'s custom two-level gesture menu: the plus navigator card
/// (level 1) plus, once a direction is activated, its action-list card (level 2)
/// flown out beside it. The two are separate floating cards — own surface and
/// height — so it reads like a cascading menu without sharing one box. The
/// active direction is scratch UI state on the [`MouseModelView`] (`None` until
/// a cell is clicked → only the plus shows), reset on popover close. Mutating it
/// re-renders the view, which re-renders this open popover's content.
pub fn gesture_overview(
    btn: ButtonId,
    view: &Entity<MouseModelView>,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let pal = theme::palette(cx);
    let active = view.read(cx).gesture_selected_dir();
    h_flex()
        .items_start()
        .gap_2()
        .child(plus_card(btn, view, active, pal, cx))
        // The flyout card only appears once a direction is activated.
        .when_some(active, |row, dir| {
            row.child(flyout_card(btn, dir, view, pal, cx))
        })
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
pub(crate) fn menu_card(pal: Palette) -> gpui::Div {
    v_flex()
        .bg(pal.surface)
        .border_1()
        .border_color(pal.border)
        .rounded(pal.card_radius)
        .shadow_md()
        .p_1p5()
}

/// The action a missing gesture-map entry actually performs at runtime, so the
/// menu never shows a swipe or tap doing something it would not.
///
/// Only an OS-hook button's raw (hand-edited, sparse) map can be missing an
/// entry — HID++ sources come fully seeded (see
/// [`AppState::current_gesture_maps`]). A tap without a `Click` entry falls
/// through to the button's plain click action ([`default_binding`]); an
/// unbound swipe does nothing.
fn sparse_gesture_fallback(btn: ButtonId, dir: GestureDirection) -> Action {
    match dir {
        GestureDirection::Click => default_binding(btn),
        _ => Action::None,
    }
}

/// Level 1: the plus navigator. `Up` on top, `Left`/`Click`/`Right` across the
/// middle, `Down` on the bottom. Each cell shows its glyph + label and bound
/// action; the `active` cell (if any) is accented. Clicking a cell activates
/// that direction (flying out the level-2 card) without committing. A footer
/// row turns gesture mode off for `btn`, demoting it back to a single action.
fn plus_card(
    btn: ButtonId,
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
                .and_then(|s| {
                    s.gesture_bindings
                        .get(&btn)
                        .and_then(|m| m.get(&d))
                        .cloned()
                })
                .unwrap_or_else(|| sparse_gesture_fallback(btn, d));
            (d, action)
        })
        .collect();

    let cell = |dir: GestureDirection| {
        direction_cell(btn, dir, &actions[&dir], active == Some(dir), view, pal)
    };

    let view_off = view.clone();
    let popover = cx.entity().downgrade();
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
        .child(divider(pal))
        .child(
            // Demote back to a single action: the Click arm becomes the
            // button's action, and the reopened popover shows the plain picker.
            menu_row("gesture-off-row", pal, false)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path("action-icons/ban.svg")
                                .size_4()
                                .flex_none()
                                .text_color(pal.text_muted),
                        )
                        .child(div().child(tr!("Turn off gestures"))),
                )
                .on_click(move |_event, window, cx| {
                    cx.update_global::<AppState, _>(|state, _| {
                        state.commit_gesture_mode(btn, false);
                    });
                    view_off.update(cx, |_, vcx| vcx.notify());
                    if let Some(p) = popover.upgrade() {
                        p.update(cx, |s, cx| s.dismiss(window, cx));
                    }
                }),
        )
        .into_any_element()
}

/// One direction's cell in the plus: a fixed-width clickable card with the
/// direction glyph + label above its bound-action label. The `active` cell is
/// accented (border + faint fill); a default binding's action is muted.
fn direction_cell(
    btn: ButtonId,
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
    } + 5 * (btn as usize);
    let header = format!("{}  {}", dir.glyph(), tr!(dir.label()));
    let action_label = tr!(current.label());
    let accessible_label = format!("{}: {action_label}", tr!(dir.label()));
    let is_default = *current == default_gesture_binding(dir);
    let view = view.clone();
    v_flex()
        .id(("gesture-cell", idx))
        .role(Role::Button)
        .aria_label(accessible_label)
        .aria_expanded(active)
        .w(px(GESTURE_CELL_W))
        .gap(px(2.))
        .px_2()
        .py_1p5()
        .rounded(pal.control_radius)
        .selected_border(active, pal)
        .selected_fill(active)
        .hover(move |s| s.bg(pal.surface_hover))
        .child(div().text_caption().text_color(pal.text_muted).child(header))
        .child(
            div()
                .text_body()
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

/// Level 2: the `dir` direction's action picker for `btn`, flown out as its
/// own card — the category-grouped catalog with the current binding checked.
/// Picking commits and stays open, so the level-1 cell + checkmark update in
/// place and the user can keep editing other directions.
fn flyout_card(
    btn: ButtonId,
    dir: GestureDirection,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let current = cx
        .try_global::<AppState>()
        .and_then(|s| {
            s.gesture_bindings
                .get(&btn)
                .and_then(|m| m.get(&dir))
                .cloned()
        })
        .unwrap_or_else(|| sparse_gesture_fallback(btn, dir));

    let view_pick = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_gesture_binding(btn, dir, action));
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
pub(crate) type PickFn = Rc<dyn Fn(Action, &mut Window, &mut App)>;

/// The action catalog grouped by [`Category`], preserving catalog order within
/// each group and first-seen order across groups.
pub(crate) fn grouped_catalog() -> Vec<(Category, Vec<Action>)> {
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
        Action::SelectAll | Action::Workflow(_) => "action-icons/list-checks.svg",
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
        Action::MissionControl | Action::ShowActionsRing => "action-icons/layout-grid.svg",
        Action::AppExpose => "action-icons/layers.svg",
        Action::PreviousDesktop => "action-icons/square-arrow-left.svg",
        Action::NextDesktop => "action-icons/square-arrow-right.svg",
        Action::ShowDesktop => "action-icons/monitor.svg",
        Action::LaunchpadShow | Action::OpenApplication(_) => "action-icons/grid-3x3.svg",
        Action::LockScreen => "action-icons/lock.svg",
        Action::Screenshot | Action::CaptureRegion => "action-icons/camera.svg",
        Action::Sleep => "action-icons/moon.svg",
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
        // Power-user actions (M1 function-key remapper). TypeText shares the
        // keyboard glyph with CustomShortcut; shell/script arms share terminal.
        Action::CustomShortcut(_) | Action::TypeText(_) => "action-icons/keyboard.svg",
        Action::RunAppleScript(_) | Action::RunShellCommand(_) => "action-icons/terminal.svg",
    }
}

/// Build the category-grouped action rows. Each row leads with the action's
/// icon, then its label; `current` adds a trailing accent check. Clicking any
/// row invokes `on_pick`. `id_prefix` disambiguates element IDs between pickers
/// that share this builder.
pub(crate) fn action_rows(
    id_prefix: &'static str,
    current: Option<&Action>,
    on_pick: &PickFn,
    pal: Palette,
) -> Vec<AnyElement> {
    let mut idx = 0usize;
    let mut children: Vec<AnyElement> = Vec::new();
    for (category, actions) in grouped_catalog() {
        let category_label = rust_i18n::t!(category.label());
        children.push(popover_section(category_label.into_owned(), pal));
        for action in actions {
            let selected = current == Some(&action);
            let label = tr!(action.label());
            let accessible_label = label.clone();
            let icon_path = action_icon_path(&action);
            let on_pick = on_pick.clone();
            let row_id = idx;
            idx += 1;
            children.push(
                menu_row((id_prefix, row_id), pal, selected)
                    .role(Role::MenuItem)
                    .aria_label(accessible_label)
                    .aria_selected(selected)
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
pub(crate) fn menu_row(
    id: impl Into<gpui::ElementId>,
    pal: Palette,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded(pal.control_radius)
        .text_body()
        .text_color(pal.text_primary)
        .selected_fill(selected)
        .hover(move |s| {
            s.bg(if selected {
                theme::accent_tint_hover()
            } else {
                pal.surface_hover
            })
        })
}

/// A group heading in a popover list, inset to line up with the rows under it.
/// Shared with the Actions Ring editor and the function-row list, which draw the
/// same list.
pub(crate) fn popover_section(label: impl Into<gpui::SharedString>, pal: Palette) -> AnyElement {
    section_label(label, pal)
        .w_full()
        .px_2()
        .pt_2()
        .pb_0p5()
        .into_any_element()
}

/// Popover title — the binding context, e.g. "Bind Back".
pub(crate) fn title(text: impl Into<gpui::SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .px_2()
        .pb_1()
        .text_subheading()
        .text_color(pal.text_muted)
        .child(text.into())
}

/// 1px hairline separating the title from the list.
pub(crate) fn divider(pal: Palette) -> impl IntoElement {
    div().mb_1().h(px(1.)).w_full().bg(pal.border)
}

/// Wrap `rows` in the height-capped, vertically scrollable list region.
pub(crate) fn scroll_list(id: &'static str, rows: Vec<AnyElement>) -> impl IntoElement {
    div()
        .id(id)
        .max_h(px(POPOVER_LIST_MAX_H))
        .overflow_y_scroll()
        .children(rows)
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;
    use openlogi_core::binding::ApplicationTarget;
    use openlogi_ui::action_icons::ActionIcons;

    use super::*;

    /// [`action_icon_path`] is exhaustive over [`Action`], so the compiler
    /// catches a missing arm — but nothing checks that the path an arm names is
    /// actually embedded, and an unembedded path renders as a blank slot with
    /// no error anywhere. [`Action::catalog`] misses the variants below:
    /// `ShowActionsRing` is `not_pickable` yet still drawn wherever it is
    /// already bound, and the parameterised arms hold the only references to
    /// the keyboard and terminal glyphs.
    #[test]
    fn every_reachable_action_icon_is_embedded() {
        let uncatalogued = [
            Action::ShowActionsRing,
            Action::SetDpiPreset(0),
            Action::CustomShortcut("Ctrl+A".parse().expect("Ctrl+A is a valid combo")),
            Action::TypeText(String::new()),
            Action::RunShellCommand(String::new()),
            Action::RunAppleScript(String::new()),
            Action::Workflow(Vec::new()),
            Action::OpenApplication(
                ApplicationTarget::new("/usr/bin/true", "true").expect("non-empty path"),
            ),
        ];
        for action in Action::catalog().into_iter().chain(uncatalogued) {
            let path = action_icon_path(&action);
            assert!(
                matches!(ActionIcons.load(path), Ok(Some(_))),
                "{path}, bound to {action:?}, is not embedded"
            );
        }
    }
}

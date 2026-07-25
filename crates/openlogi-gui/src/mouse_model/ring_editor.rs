//! The Action Ring's full-page editor — the Options+ customization page
//! shape, not a popover: a back arrow, the ring drawn large with every
//! circle labelled by its bound action, and a persistent action panel on the
//! right editing the selected slot.
//!
//! Clicking the ring's hotspot or label card on the mouse diagram swaps the
//! diagram for this page (scratch state on [`MouseModelView`]); the back
//! arrow swaps back. Folders drill in: clicking an already-selected folder
//! circle (or the panel's "Open Folder" row) shows the folder's contents on
//! the same canvas — empty positions render as dashed "+" targets — and the
//! back arrow then pops to the top level first.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, BorrowAppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{h_flex, v_flex};
use openlogi_core::binding::{Action, RingSlot, default_ring_binding};

use crate::mouse_model::picker::{
    PickFn, action_icon_path, action_rows, payload_editor_row, section_header,
};
use crate::mouse_model::view::{MouseModelView, localized_action_label};
use crate::state::AppState;
use crate::theme::{ACCENT_BLUE, Palette};
use crate::windows::ring_action_editor::{EditTarget, PayloadKind};

/// Radius of the circle the editor's slot buttons sit on.
const EDIT_RADIUS: f32 = 110.;
/// Diameter of one slot circle on the canvas.
const EDIT_CIRCLE: f32 = 48.;
/// Diameter of the decorative centre circle.
const EDIT_CENTER: f32 = 36.;
/// Width reserved for a floating label pill column beside a circle.
const PILL_W: f32 = 140.;
/// Gap between a circle and its label pill.
const PILL_GAP: f32 = 10.;
/// Vertical room above / below the ring for the North / South pills.
const PILL_V: f32 = 44.;
/// The canvas is a wide, short rectangle — pills extend sideways, so the
/// height only needs the ring plus the North/South pill rows. A square at
/// pill width was taller than the content area and clipped.
const CANVAS_W: f32 = 2. * (EDIT_RADIUS + EDIT_CIRCLE / 2. + PILL_GAP + PILL_W);
const CANVAS_H: f32 = 2. * (EDIT_RADIUS + EDIT_CIRCLE / 2. + PILL_V);
/// Fixed width of the right-hand action panel.
const PANEL_W: f32 = 260.;
/// Height cap for the action panel's scrolling list.
const PANEL_LIST_H: f32 = 360.;

/// One canvas position: the slot, what's bound there (`None` = an empty
/// folder position rendered as a dashed add target), and whether it is the
/// active selection.
struct CanvasSlot {
    slot: RingSlot,
    action: Option<Action>,
    selected: bool,
}

/// Build the full-page editor. Rendered by [`MouseModelView`] in place of
/// the mouse diagram while its `ring_editor_open` scratch flag is set.
///
/// `selected` / `folder_open` come from the caller's `&self` — this runs
/// inside the view's own `render`, where `view.read(cx)` would panic
/// ("cannot read … while it is already being updated"); `view` is only
/// cloned into event closures.
pub(crate) fn page(
    selected: Option<RingSlot>,
    folder_open: Option<RingSlot>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let selected = selected.unwrap_or(RingSlot::North);
    let actions: BTreeMap<RingSlot, Action> = cx
        .try_global::<AppState>()
        .map(|s| s.ring_slots_for_current().into_iter().collect())
        .unwrap_or_default();

    // Drilled-in state survives the folder being replaced from elsewhere
    // (another window, a config reload): fall back to the top level unless
    // the slot still holds a folder.
    let folder_open =
        folder_open.filter(|slot| matches!(actions.get(slot), Some(Action::Folder(_))));
    let folder_items: Option<&BTreeMap<RingSlot, Action>> =
        folder_open.and_then(|slot| match actions.get(&slot) {
            Some(Action::Folder(items)) => Some(items),
            _ => None,
        });

    let canvas_slots: Vec<CanvasSlot> = RingSlot::ALL
        .into_iter()
        .map(|slot| CanvasSlot {
            slot,
            action: match folder_items {
                Some(items) => items
                    .get(&slot)
                    .filter(|action| !matches!(action, Action::None))
                    .cloned(),
                None => Some(
                    actions
                        .get(&slot)
                        .cloned()
                        .unwrap_or_else(|| default_ring_binding(slot)),
                ),
            },
            selected: slot == selected,
        })
        .collect();

    let title: SharedString = match folder_open {
        Some(slot) => format!(
            "{}  {}  ›  {}",
            tr!("Action Ring"),
            slot.glyph(),
            tr!("Folder")
        )
        .into(),
        None => tr!("Action Ring"),
    };

    let view_back = view.clone();
    let back = h_flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .id("ring-editor-back")
                .p_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(pal.surface))
                .on_click(move |_, _, cx| {
                    view_back.update(cx, |v, vcx| {
                        // Pop the folder first; only a top-level back leaves
                        // the editor.
                        if folder_open.is_some() {
                            v.set_ring_folder_open(None);
                            v.set_ring_selected_slot(folder_open);
                        } else {
                            v.set_ring_editor_open(false);
                            v.set_ring_selected_slot(None);
                        }
                        vcx.notify();
                    });
                })
                .child(
                    svg()
                        .path("action-icons/arrow-left.svg")
                        .size_5()
                        .text_color(pal.text_primary),
                ),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(pal.text_muted)
                .child(tr!("8 actions")),
        );

    // Header on top; below it the canvas and panel side by side, the pair
    // centred as one block so the page reads composed at any window size.
    v_flex()
        .size_full()
        .gap_2()
        .child(back)
        .child(
            div().flex_1().flex().items_center().justify_center().child(
                h_flex()
                    .items_center()
                    .gap_6()
                    .child(canvas(&canvas_slots, folder_open, view, pal))
                    .child(action_panel(selected, folder_open, &actions, view, pal)),
            ),
        )
        .into_any_element()
}

/// The ring canvas: eight slot circles on a ring plus the decorative centre.
/// Empty folder positions are dashed "+" add targets; the selected slot (and
/// its pill) is accented.
fn canvas(
    slots: &[CanvasSlot],
    folder_open: Option<RingSlot>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let (cw, ch) = (CANVAS_W / 2., CANVAS_H / 2.);
    let mut layer = div().relative().flex_none().w(px(CANVAS_W)).h(px(CANVAS_H));

    // Decorative centre — the popup's ✕ (or the folder's ← back) is only in
    // the on-screen ring; here it is orientation.
    layer = layer.child(
        div()
            .absolute()
            .left(px(cw - EDIT_CENTER / 2.))
            .top(px(ch - EDIT_CENTER / 2.))
            .w(px(EDIT_CENTER))
            .h(px(EDIT_CENTER))
            .rounded_full()
            .bg(pal.surface)
            .border_1()
            .border_color(pal.border)
            .flex()
            .items_center()
            .justify_center()
            .text_color(pal.text_muted)
            .text_sm()
            .child(if folder_open.is_some() { "←" } else { "✕" }),
    );

    for (idx, canvas_slot) in slots.iter().enumerate() {
        let slot = canvas_slot.slot;
        let is_selected = canvas_slot.selected;
        let angle = slot.angle_degrees().to_radians();
        let bx = cw + EDIT_RADIUS * angle.sin() - EDIT_CIRCLE / 2.;
        let by = ch - EDIT_RADIUS * angle.cos() - EDIT_CIRCLE / 2.;

        // Clicking selects; clicking an already-selected folder drills in.
        let drills = folder_open.is_none()
            && is_selected
            && matches!(canvas_slot.action, Some(Action::Folder(_)));
        let view_pick = view.clone();
        let on_activate = move |cx: &mut gpui::App| {
            view_pick.update(cx, |v, vcx| {
                if drills {
                    v.set_ring_folder_open(Some(slot));
                    v.set_ring_selected_slot(Some(RingSlot::North));
                } else {
                    v.set_ring_selected_slot(Some(slot));
                }
                vcx.notify();
            });
        };

        let circle = div()
            .id(("ring-editor-slot", idx))
            .absolute()
            .left(px(bx))
            .top(px(by))
            .w(px(EDIT_CIRCLE))
            .h(px(EDIT_CIRCLE))
            .rounded_full()
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click({
                let on_activate = on_activate.clone();
                move |_, _, cx| on_activate(cx)
            });
        let circle = match &canvas_slot.action {
            Some(action) => circle
                .bg(pal.surface)
                .border_2()
                .border_color(if is_selected {
                    rgb(ACCENT_BLUE).into()
                } else {
                    pal.border
                })
                .hover(|s| s.border_color(rgb(ACCENT_BLUE)))
                .child(
                    svg()
                        .path(action_icon_path(action))
                        .size_6()
                        .text_color(pal.text_primary),
                ),
            // An empty folder position: a dashed add target.
            None => circle
                .border_2()
                .border_dashed()
                .border_color(if is_selected {
                    rgb(ACCENT_BLUE).into()
                } else {
                    pal.border
                })
                .hover(|s| s.border_color(rgb(ACCENT_BLUE)))
                .child(div().text_lg().text_color(pal.text_muted).child("+")),
        };
        layer = layer.child(circle);

        if let Some(action) = &canvas_slot.action {
            layer = layer.child(label_pill(
                slot,
                action,
                (bx, by),
                is_selected,
                on_activate,
                pal,
            ));
        }
    }
    layer.into_any_element()
}

/// The floating label pill beside a slot circle, placed outward from the
/// ring: east-side slots label to the right, west-side to the left, North
/// above and South below — matching the Options+ canvas. Clicking the pill
/// selects (or drills, same as its circle).
fn label_pill(
    slot: RingSlot,
    action: &Action,
    (bx, by): (f32, f32),
    selected: bool,
    on_activate: impl Fn(&mut gpui::App) + Clone + 'static,
    pal: Palette,
) -> AnyElement {
    let idx = RingSlot::ALL
        .iter()
        .position(|s| *s == slot)
        .unwrap_or_default();
    let pill = div()
        .id(("ring-editor-pill", idx))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(pal.surface)
        .border_1()
        .border_color(if selected {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .cursor_pointer()
        .hover(|s| s.border_color(rgb(ACCENT_BLUE)))
        .text_sm()
        .text_color(pal.text_primary)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .max_w(px(PILL_W))
        .child(localized_action_label(action))
        .on_click(move |_, _, cx| on_activate(cx));

    let holder = div().absolute().w(px(PILL_W)).flex();
    let mid_y = by + EDIT_CIRCLE / 2. - 13.;
    match slot {
        RingSlot::NorthEast | RingSlot::East | RingSlot::SouthEast => holder
            .left(px(bx + EDIT_CIRCLE + PILL_GAP))
            .top(px(mid_y))
            .justify_start(),
        RingSlot::SouthWest | RingSlot::West | RingSlot::NorthWest => holder
            .left(px(bx - PILL_GAP - PILL_W))
            .top(px(mid_y))
            .justify_end(),
        RingSlot::North => holder
            .left(px(bx + EDIT_CIRCLE / 2. - PILL_W / 2.))
            .top(px(by - 34.))
            .justify_center(),
        RingSlot::South => holder
            .left(px(bx + EDIT_CIRCLE / 2. - PILL_W / 2.))
            .top(px(by + EDIT_CIRCLE + PILL_GAP))
            .justify_center(),
    }
    .child(pill)
    .into_any_element()
}

/// The right-hand action panel for the selected slot: its compass name on
/// top, then the full categorized catalog plus the CUSTOM editor rows. At
/// the top level a selected folder gains an "Open Folder" row and a
/// "Folder…" row converts a plain slot; inside a folder the rows commit to
/// the folder's sub-slot (no nesting offered).
fn action_panel(
    selected: RingSlot,
    folder_open: Option<RingSlot>,
    actions: &BTreeMap<RingSlot, Action>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let target = match folder_open {
        Some(folder) => EditTarget::FolderSlot {
            folder,
            sub: selected,
        },
        None => EditTarget::Slot(selected),
    };
    let current = match folder_open {
        Some(folder) => match actions.get(&folder) {
            Some(Action::Folder(items)) => items.get(&selected).cloned().unwrap_or(Action::None),
            _ => Action::None,
        },
        None => actions
            .get(&selected)
            .cloned()
            .unwrap_or_else(|| default_ring_binding(selected)),
    };

    let view_pick = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        target.commit(action, cx);
        view_pick.update(cx, |_, vcx| vcx.notify());
    });

    let mut rows: Vec<AnyElement> = Vec::new();
    // A selected top-level folder: entering it is the panel's first offer.
    if folder_open.is_none() && matches!(current, Action::Folder(_)) {
        let view_open = view.clone();
        rows.push(
            open_folder_row(pal, move |cx| {
                view_open.update(cx, |v, vcx| {
                    v.set_ring_folder_open(Some(selected));
                    v.set_ring_selected_slot(Some(RingSlot::North));
                    vcx.notify();
                });
            })
            .into_any_element(),
        );
    }
    rows.extend(action_rows(
        "ring-editor-action",
        Some(&current),
        &on_pick,
        pal,
    ));
    rows.push(section_header(&rust_i18n::t!("CUSTOM"), pal));
    for (idx, kind) in [
        PayloadKind::Run,
        PayloadKind::PasteText,
        PayloadKind::Shortcut,
    ]
    .into_iter()
    .enumerate()
    {
        rows.push(payload_editor_row(target, kind, idx, &current, pal));
    }
    if folder_open.is_none() {
        let view_convert = view.clone();
        let is_folder = matches!(current, Action::Folder(_));
        rows.push(
            folder_convert_row(is_folder, pal, move |cx| {
                cx.update_global::<AppState, _>(|state, _| {
                    state.convert_ring_slot_to_folder(selected);
                });
                view_convert.update(cx, |v, vcx| {
                    v.set_ring_folder_open(Some(selected));
                    v.set_ring_selected_slot(Some(RingSlot::North));
                    vcx.notify();
                });
            })
            .into_any_element(),
        );
    }

    v_flex()
        .w(px(PANEL_W))
        .flex_none()
        .bg(pal.surface)
        .border_1()
        .border_color(pal.border)
        .rounded_lg()
        .p_1p5()
        .child(
            div()
                .px_2()
                .py_1p5()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("{}  {}", selected.glyph(), tr!(selected.label()))),
        )
        .child(
            div()
                .id("ring-editor-panel-scroll")
                .max_h(px(PANEL_LIST_H))
                .overflow_y_scroll()
                .child(v_flex().children(rows)),
        )
        .into_any_element()
}

/// "Open Folder →" — the drill-in row shown while a folder is selected.
fn open_folder_row(pal: Palette, on_open: impl Fn(&mut gpui::App) + 'static) -> impl IntoElement {
    h_flex()
        .id("ring-editor-open-folder")
        .w_full()
        .items_center()
        .justify_between()
        .px_2()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .hover(move |s| s.bg(pal.surface_hover))
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path("action-icons/folder.svg")
                        .size_4()
                        .flex_none()
                        .text_color(rgb(ACCENT_BLUE)),
                )
                .child(tr!("Open Folder")),
        )
        .child(div().text_color(pal.text_muted).child("→"))
        .on_click(move |_, _, cx| on_open(cx))
}

/// "Folder…" — converts the selected top-level slot into a folder (keeping
/// its action as the folder's North entry) and drills straight in.
fn folder_convert_row(
    is_folder: bool,
    pal: Palette,
    on_convert: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    h_flex()
        .id("ring-editor-make-folder")
        .w_full()
        .items_center()
        .justify_between()
        .px_2()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .hover(move |s| s.bg(pal.surface_hover))
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path("action-icons/folder.svg")
                        .size_4()
                        .flex_none()
                        .text_color(pal.text_muted),
                )
                .child(format!("{}…", tr!("Folder"))),
        )
        .when(is_folder, |s| {
            s.child(
                gpui_component::Icon::new(gpui_component::IconName::Check)
                    .size_3()
                    .text_color(rgb(ACCENT_BLUE)),
            )
        })
        .on_click(move |_, _, cx| on_convert(cx))
}

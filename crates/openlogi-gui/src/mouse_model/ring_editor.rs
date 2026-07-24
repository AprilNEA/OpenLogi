//! The Action Ring's full-page editor — the Options+ customization page
//! shape, not a popover: a back arrow, the ring drawn large with every
//! circle labelled by its bound action, and a persistent action panel on the
//! right editing the selected slot.
//!
//! Clicking the ring's hotspot or label card on the mouse diagram swaps the
//! diagram for this page (scratch state on [`MouseModelView`]); the back
//! arrow swaps back. Selection is the same `ring_active_slot` scratch state
//! the old flyout used, so the action panel and the canvas highlight always
//! agree.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, BorrowAppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement as _, Styled, div, px, rgb, svg,
};
use gpui_component::{h_flex, v_flex};
use openlogi_core::binding::{Action, RingSlot, default_ring_binding};

use crate::mouse_model::picker::{
    PickFn, action_icon_path, action_rows, payload_editor_row, section_header,
};
use crate::mouse_model::view::{MouseModelView, localized_action_label};
use crate::state::AppState;
use crate::theme::{ACCENT_BLUE, Palette};
use crate::windows::ring_action_editor::PayloadKind;

/// Radius of the circle the editor's slot buttons sit on.
const EDIT_RADIUS: f32 = 150.;
/// Diameter of one slot circle on the canvas.
const EDIT_CIRCLE: f32 = 56.;
/// Diameter of the decorative centre ✕.
const EDIT_CENTER: f32 = 40.;
/// Square canvas the ring is drawn in (labels hang outside the circle ring,
/// inside this box).
const CANVAS: f32 = 440.;
/// Width reserved for a floating label pill column beside a circle.
const PILL_W: f32 = 170.;
/// Gap between a circle and its label pill.
const PILL_GAP: f32 = 10.;
/// Fixed width of the right-hand action panel.
const PANEL_W: f32 = 260.;
/// Height cap for the action panel's scrolling list.
const PANEL_LIST_H: f32 = 420.;

/// Build the full-page editor. Rendered by [`MouseModelView`] in place of the
/// mouse diagram while its `ring_editor_open` scratch flag is set.
pub(crate) fn page(
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let selected = view
        .read(cx)
        .ring_selected_slot()
        .unwrap_or(RingSlot::North);
    let actions: BTreeMap<RingSlot, Action> = cx
        .try_global::<AppState>()
        .map(|s| s.ring_slots_for_current().into_iter().collect())
        .unwrap_or_default();

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
                        v.set_ring_editor_open(false);
                        v.set_ring_selected_slot(None);
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
                .child(tr!("Action Ring")),
        )
        .child(
            div()
                .text_sm()
                .text_color(pal.text_muted)
                .child(tr!("8 actions")),
        );

    h_flex()
        .size_full()
        .items_start()
        .gap_6()
        .child(
            v_flex().flex_1().gap_2().child(back).child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(canvas(&actions, selected, view, pal)),
            ),
        )
        .child(action_panel(selected, &actions, view, pal))
        .into_any_element()
}

/// The ring canvas: eight labelled slot circles on a ring, the decorative
/// centre ✕, and the selected slot accented — the Options+ customization
/// canvas.
fn canvas(
    actions: &BTreeMap<RingSlot, Action>,
    selected: RingSlot,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let c = CANVAS / 2.;
    let mut layer = div().relative().w(px(CANVAS)).h(px(CANVAS));

    // Decorative centre ✕ — the popup's cancel button, here just orientation.
    layer = layer.child(
        div()
            .absolute()
            .left(px(c - EDIT_CENTER / 2.))
            .top(px(c - EDIT_CENTER / 2.))
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
            .child("✕"),
    );

    for (idx, slot) in RingSlot::ALL.into_iter().enumerate() {
        let action = actions
            .get(&slot)
            .cloned()
            .unwrap_or_else(|| default_ring_binding(slot));
        let angle = slot.angle_degrees().to_radians();
        let bx = c + EDIT_RADIUS * angle.sin() - EDIT_CIRCLE / 2.;
        let by = c - EDIT_RADIUS * angle.cos() - EDIT_CIRCLE / 2.;
        let is_selected = slot == selected;

        let view_pick = view.clone();
        let circle = div()
            .id(("ring-editor-slot", idx))
            .absolute()
            .left(px(bx))
            .top(px(by))
            .w(px(EDIT_CIRCLE))
            .h(px(EDIT_CIRCLE))
            .rounded_full()
            .bg(pal.surface)
            .border_2()
            .border_color(if is_selected {
                rgb(ACCENT_BLUE).into()
            } else {
                pal.border
            })
            .cursor_pointer()
            .hover(|s| s.border_color(rgb(ACCENT_BLUE)))
            .flex()
            .items_center()
            .justify_center()
            .on_click(move |_, _, cx| {
                view_pick.update(cx, |v, vcx| {
                    v.set_ring_selected_slot(Some(slot));
                    vcx.notify();
                });
            })
            .child(
                svg()
                    .path(action_icon_path(&action))
                    .size_6()
                    .text_color(pal.text_primary),
            );
        layer = layer
            .child(circle)
            .child(label_pill(slot, &action, (bx, by), is_selected, pal));
    }
    layer.into_any_element()
}

/// The floating label pill beside a slot circle, placed outward from the
/// ring: east-side slots label to the right, west-side to the left, North
/// above and South below — matching the Options+ canvas.
fn label_pill(
    slot: RingSlot,
    action: &Action,
    (bx, by): (f32, f32),
    selected: bool,
    pal: Palette,
) -> AnyElement {
    let pill = div()
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
        .text_sm()
        .text_color(pal.text_primary)
        .child(localized_action_label(action));

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
            .top(px(by - 36.))
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
/// top, then the full categorized catalog plus the CUSTOM editor rows —
/// the same rows the popover flyout used, hosted persistently.
fn action_panel(
    slot: RingSlot,
    actions: &BTreeMap<RingSlot, Action>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let current = actions
        .get(&slot)
        .cloned()
        .unwrap_or_else(|| default_ring_binding(slot));

    let view_pick = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_ring_binding(slot, action));
        view_pick.update(cx, |_, vcx| vcx.notify());
    });

    let mut rows = action_rows("ring-editor-action", Some(&current), &on_pick, pal);
    rows.push(section_header(&rust_i18n::t!("CUSTOM"), pal));
    for (idx, kind) in [PayloadKind::Run, PayloadKind::PasteText]
        .into_iter()
        .enumerate()
    {
        rows.push(payload_editor_row(slot, kind, idx, &current, pal));
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
                .child(format!("{}  {}", slot.glyph(), tr!(slot.label()))),
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

//! The `Classic` theme: a dark disc of round icon slots, with the hovered
//! slot's label under the cancel button. It opens and closes without motion.

use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement as _,
    Styled, div, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
pub(super) use openlogi_ui::ring_style::classic::PANEL_SIZE;
use openlogi_ui::ring_style::classic::{
    CANCEL_GLYPH, CANCEL_GLYPH_SIZE, CANCEL_RESTING, CANCEL_SIZE, DISC_INSET, GLYPH, LABEL,
    LABEL_OFFSET, LABEL_WIDTH, PANEL, RADIUS, SLOT_GLYPH, SLOT_RESTING, SLOT_SIZE, selected_border,
    selected_fill,
};

use super::RingView;
use crate::ipc::OverlayCommand;

impl RingView {
    /// The disc, which fills its [`PANEL_SIZE`] window.
    pub(super) fn classic_panel(&self, cx: &mut Context<Self>) -> Div {
        let session_id = self.invocation.session_id;
        let hovered_label = self.hovered_caption();
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, cx))
            .collect::<Vec<_>>();

        div()
            .relative()
            .size(px(PANEL_SIZE))
            .child(
                div()
                    .absolute()
                    .left(px(DISC_INSET))
                    .top(px(DISC_INSET))
                    .size(px(PANEL_SIZE - 2.0 * DISC_INSET))
                    .rounded_full()
                    .bg(PANEL)
                    .shadow_lg(),
            )
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px(PANEL_SIZE / 2.0 - CANCEL_SIZE / 2.0))
                    .top(px(PANEL_SIZE / 2.0 - CANCEL_SIZE / 2.0))
                    .size(px(CANCEL_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(CANCEL_RESTING)
                    .text_color(CANCEL_GLYPH)
                    .cursor_pointer()
                    .child(
                        svg()
                            .path(RING_CANCEL_ICON)
                            .size(px(CANCEL_GLYPH_SIZE))
                            .flex_none(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close(OverlayCommand::Cancel { session_id }, window, cx);
                    })),
            )
            .when_some(hovered_label, |ring, label| {
                ring.child(
                    div()
                        .absolute()
                        .left(px(PANEL_SIZE / 2.0 - LABEL_WIDTH / 2.0))
                        .top(px(PANEL_SIZE / 2.0 + LABEL_OFFSET))
                        .w(px(LABEL_WIDTH))
                        .text_center()
                        .text_sm()
                        .text_color(LABEL)
                        .child(label),
                )
            })
    }

    fn slot_element(
        &self,
        slot: ActionRingSlot,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let (left, top) = slot.placement(PANEL_SIZE, RADIUS, SLOT_SIZE);
        let session_id = self.invocation.session_id;
        Some(
            div()
                .id(("ring-slot", slot.index()))
                .absolute()
                .left(px(left))
                .top(px(top))
                .size(px(SLOT_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(if selected {
                    selected_fill()
                } else {
                    SLOT_RESTING
                })
                .when(selected, |slot| {
                    slot.border_2().border_color(selected_border())
                })
                .shadow_md()
                .text_color(GLYPH)
                .cursor_pointer()
                .child(svg().path(icon_path).size(px(SLOT_GLYPH)).text_color(GLYPH))
                .on_hover(cx.listener(move |this, hovered, _, cx| {
                    this.hover(slot, *hovered, cx);
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close(OverlayCommand::Activate { session_id, slot }, window, cx);
                }))
                .into_any_element(),
        )
    }
}

/// Centre of `slot` within the ring window, for tests that aim the pointer
/// at it.
#[cfg(test)]
pub(super) fn slot_center(slot: ActionRingSlot) -> (f32, f32) {
    let (left, top) = slot.placement(PANEL_SIZE, RADIUS, SLOT_SIZE);
    (left + SLOT_SIZE / 2.0, top + SLOT_SIZE / 2.0)
}

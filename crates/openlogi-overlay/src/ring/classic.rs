//! The `Classic` theme: a dark disc of round icon slots, with the hovered
//! slot's label under the cancel button. It opens and closes without motion.

use gpui::{
    Context, Div, Hsla, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;

use super::RingView;
use crate::ipc::OverlayCommand;

/// The disc's edge, which is the ring window's edge for this theme.
pub(super) const PANEL_SIZE: f32 = 360.0;
const SLOT_SIZE: f32 = 54.0;
const RADIUS: f32 = 122.0;

/// The ring's own neutral scale. It floats over whatever is on the desktop, so
/// unlike the settings app it cannot take its surfaces from the OS appearance —
/// it commits to a dark panel and rides its own contrast. Only the accent is
/// shared (`openlogi_ui::color`); these greys are local by nature.
const PANEL: Hsla = neutral(0.06, 0.82);
const SLOT_RESTING: Hsla = neutral(0.16, 0.98);
const CANCEL_RESTING: Hsla = neutral(0.20, 0.98);
const GLYPH: Hsla = neutral(0.98, 1.0);
const LABEL: Hsla = neutral(0.94, 1.0);
const CANCEL_GLYPH: Hsla = neutral(0.82, 1.0);

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// The accent deepened for the dark panel: the brand lightness sits too close to
/// the white glyph a selected slot carries, so the fill drops to `0.48` and the
/// ring around it rises to `0.78`. Both keep the brand hue and saturation.
const SELECTED_FILL_L: f32 = 0.48;
const SELECTED_BORDER_L: f32 = 0.78;

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
                    .left(px(18.0))
                    .top(px(18.0))
                    .size(px(PANEL_SIZE - 36.0))
                    .rounded_full()
                    .bg(PANEL)
                    .shadow_lg(),
            )
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px(PANEL_SIZE / 2.0 - 24.0))
                    .top(px(PANEL_SIZE / 2.0 - 24.0))
                    .size(px(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(CANCEL_RESTING)
                    .text_color(CANCEL_GLYPH)
                    .cursor_pointer()
                    .child(svg().path(RING_CANCEL_ICON).size(px(20.0)).flex_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close(OverlayCommand::Cancel { session_id }, window, cx);
                    })),
            )
            .when_some(hovered_label, |ring, label| {
                ring.child(
                    div()
                        .absolute()
                        .left(px(PANEL_SIZE / 2.0 - 80.0))
                        .top(px(PANEL_SIZE / 2.0 + 34.0))
                        .w(px(160.0))
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
                    color::accent_at_lightness(SELECTED_FILL_L)
                } else {
                    SLOT_RESTING
                })
                .when(selected, |slot| {
                    slot.border_2()
                        .border_color(color::accent_at_lightness(SELECTED_BORDER_L))
                })
                .shadow_md()
                .text_color(GLYPH)
                .cursor_pointer()
                .child(svg().path(icon_path).size(px(22.0)).text_color(GLYPH))
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

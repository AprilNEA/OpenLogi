//! The `Classic` theme: a dark disc of round icon slots, with the hovered
//! slot's label under the cancel button. It opens and closes without motion.

use gpui::{
    Context, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;

use super::RingView;
use super::skin::slot_caption;
use crate::ipc::OverlayCommand;
use crate::session;

/// The disc's designed edge, drawn centred in the ring's window.
const PANEL_SIZE: f32 = 360.0;
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
    /// The disc, centred in an `edge`-pixel square (see [`super::panel_edge`]).
    // shortcut: below 360 px the disc overhangs its square instead of scaling;
    // scale it like the motion skin if displays that short ever matter.
    pub(super) fn classic_panel(&self, edge: Pixels, cx: &mut Context<Self>) -> Div {
        let inset = (f32::from(edge) - PANEL_SIZE) / 2.0;
        div().relative().size(edge).child(
            div()
                .absolute()
                .left(px(inset))
                .top(px(inset))
                .child(self.disc(cx)),
        )
    }

    fn disc(&self, cx: &mut Context<Self>) -> Div {
        let session_id = self.invocation.session_id;
        let center_commands = self.commands.clone();
        let hovered_label = self
            .hovered
            .and_then(|slot| self.invocation.slots.get(&slot))
            .map(slot_caption);
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
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        session::dismiss_from_window(cx, window, session_id);
                    }),
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
        let activate = self.commands.clone();
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
                    if *hovered && this.hovered != Some(slot) {
                        this.hovered = Some(slot);
                        let _ = this
                            .commands
                            .send(OverlayCommand::Hover { session_id, slot });
                        cx.notify();
                    } else if !*hovered && this.hovered == Some(slot) {
                        this.hovered = None;
                        cx.notify();
                    }
                }))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                    session::dismiss_from_window(cx, window, session_id);
                })
                .into_any_element(),
        )
    }
}

/// Centre of `slot` within a full-size ring window, for tests that aim the
/// pointer at it.
#[cfg(test)]
pub(super) fn slot_center(slot: ActionRingSlot) -> (f32, f32) {
    let inset = (super::WINDOW_SIZE - PANEL_SIZE) / 2.0;
    let (left, top) = slot.placement(PANEL_SIZE, RADIUS, SLOT_SIZE);
    (
        inset + left + SLOT_SIZE / 2.0,
        inset + top + SLOT_SIZE / 2.0,
    )
}

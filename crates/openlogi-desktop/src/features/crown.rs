//! The Craft crown remapper — the Crown tab body.
//!
//! Unlike the mouse Buttons tab, there is no hardware diagram: the crown's six
//! controls ([`ButtonId::CROWN_CONTROLS`] — touch, press, plain rotation, and
//! press-held rotation) are fixed and always present, so this is a plain list
//! of expandable rows rather than clickable hotspots on a photo. Each row
//! opens the same action catalog the mouse picker uses. Bindings are
//! per-device (`AppState::commit_binding`), the same as the mouse Buttons tab
//! — unlike keyboard F-key bindings, which are global.

use std::rc::Rc;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, Role, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, svg,
};
use gpui_component::{Icon, IconName, Selectable as _, h_flex, v_flex};
use openlogi_core::binding::{Action, ButtonId, default_binding};

use crate::features::mouse::picker::{
    PickFn, action_rows, compact_panel, divider, editor_scroll_list,
};
use crate::state::{AppState, StateEvent};
use crate::ui::action::localized_action_label;
use crate::ui::components::MenuRow;
use crate::ui::theme::{self, Palette, Typography as _};

/// Vendored glyph for one crown control. Exhaustive so a new control must
/// deliberately choose an icon; rotation direction and its press-held variant
/// share an icon, distinguished by the row's label.
fn control_icon(button: ButtonId) -> &'static str {
    match button {
        ButtonId::CrownTouch => "action-icons/circle-dot.svg",
        ButtonId::Crown => "action-icons/mouse-pointer-click.svg",
        ButtonId::CrownRotateClockwise | ButtonId::CrownPressRotateClockwise => {
            "action-icons/rotate-cw.svg"
        }
        ButtonId::CrownRotateCounterclockwise | ButtonId::CrownPressRotateCounterclockwise => {
            "action-icons/rotate-ccw.svg"
        }
        // CROWN_CONTROLS is the only caller; every other ButtonId variant
        // stays unreachable through this function.
        _ => "action-icons/ban.svg",
    }
}

/// The crown remapper view: a fixed list of expandable rows, one per
/// [`ButtonId::CROWN_CONTROLS`] entry.
pub struct CrownPanel {
    /// The control whose action picker is open, or `None` when every row is
    /// collapsed.
    expanded: Option<ButtonId>,
    _state_obs: Subscription,
}

impl CrownPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(&AppState::global(cx), |_view, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::BindingsChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            expanded: None,
            _state_obs: state_obs,
        }
    }

    /// Toggle a row's picker open/closed; opening one row closes any other.
    fn toggle(&mut self, button: ButtonId, cx: &mut Context<Self>) {
        self.expanded = if self.expanded == Some(button) {
            None
        } else {
            Some(button)
        };
        cx.notify();
    }
}

impl Render for CrownPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let bindings = AppState::try_read(cx).map(AppState::button_bindings);
        let expanded = self.expanded;
        let view = cx.entity();

        v_flex()
            .gap_2()
            .children(
                ButtonId::CROWN_CONTROLS
                    .into_iter()
                    .enumerate()
                    .map(|(index, button)| {
                        let action = bindings
                            .and_then(|bindings| bindings.get(&button))
                            .cloned()
                            .unwrap_or_else(|| default_binding(button));
                        control_row(index, button, &action, expanded == Some(button), &view, pal)
                    }),
            )
    }
}

fn control_row(
    index: usize,
    button: ButtonId,
    action: &Action,
    expanded: bool,
    view: &Entity<CrownPanel>,
    pal: Palette,
) -> gpui::Div {
    let label = tr!(button.translation_key());
    let current_label = localized_action_label(action);
    let toggle_view = view.clone();
    let header = MenuRow::new(("crown-control", index))
        .selected(expanded)
        .role(Role::Button)
        .aria_label(label.clone())
        .child(
            h_flex()
                .min_w_0()
                .flex_1()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(control_icon(button))
                        .size_4()
                        .flex_none()
                        .text_color(pal.text_muted),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(div().text_body().truncate().child(label))
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .truncate()
                                .child(current_label),
                        ),
                ),
        )
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size_3()
            .text_color(pal.text_muted),
        )
        .on_click(move |_event, _window, cx| {
            toggle_view.update(cx, |panel, cx| panel.toggle(button, cx));
        });

    let card = compact_panel(pal).child(header);
    if expanded {
        let observer = view.clone();
        let on_pick: PickFn = Rc::new(move |action, _window, cx| {
            AppState::update_bindings(cx, |state| state.commit_binding(button, action));
            observer.update(cx, |panel, cx| {
                panel.expanded = None;
                cx.notify();
            });
        });
        card.child(divider(pal)).child(editor_scroll_list(
            "crown-action-list",
            action_rows(button.label(), Some(action), &on_pick, pal),
        ))
    } else {
        card
    }
}

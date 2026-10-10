//! The binding labels beside the mouse render: what each control card says and how it is placed in the side gutter.

use super::super::geometry::LABEL_H;
use super::super::hotspots::MouseControlId;
use super::super::leader_lines::{Label, Side};
use super::super::thumbwheel::ThumbwheelPreset;
use crate::features::binding_editor::{GESTURE_BUTTON_ICON, action_icon_path};
use crate::ui::action::localized_action_label;
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};
use gpui::{
    App, ElementId, Entity, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
    svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{Icon, IconName, h_flex};
use openlogi_core::binding::{Action, ButtonId, default_binding};

use super::{LABEL_W, ModelRect, MouseModelView, SIDE_GAP, set_control_hovered};

/// Position a selectable control card at the label's slot in the side gutter.
/// Selection updates the fixed inspector; labels never own editor overlays.
pub(super) fn label_control(
    idx: usize,
    label: Label,
    binding: BindingLabel,
    highlighted: bool,
    model: ModelRect,
    selected: bool,
    view: &Entity<MouseModelView>,
) -> gpui::Div {
    let x = match label.side {
        Side::Left => model.left - SIDE_GAP - LABEL_W,
        Side::Right => model.left + model.width + SIDE_GAP,
    };
    let view = view.clone();
    let control = label.id;
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        highlighted,
        selected,
        view,
    };
    div()
        .absolute()
        .left(px(x))
        .top(px(label.y - LABEL_H / 2.))
        .w(px(LABEL_W))
        .h(px(LABEL_H))
        .debug_selector(move || format!("label-card-{control:?}"))
        .child(trigger)
}

pub(super) struct BindingLabel {
    text: gpui::SharedString,
    /// Vendored action-icon asset path (see [`action_icon_path`]) for the
    /// card's leading glyph. Every constructor currently supplies one; the
    /// `Option` is the seam for icon-less bindings.
    icon: Option<&'static str>,
}

impl BindingLabel {
    /// The card's value row: the leading action icon, the binding text, and
    /// the trailing chevron, all tinted with `color`.
    fn row(self, color: Hsla, pal: theme::Palette) -> gpui::Div {
        h_flex()
            .items_center()
            .gap_2()
            // Leading action icon (same glyph as the picker rows), tinted with
            // the value so it tracks the default / set / highlighted state.
            .when_some(self.icon, |row, path| {
                row.child(svg().path(path).size_4().flex_none().text_color(color))
            })
            .child(
                // Shrink + ellipsis so a long action name (e.g. "Mission
                // Control") doesn't push the chevron out of the fixed card.
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_body()
                    .text_color(color)
                    .child(self.text),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .size_3()
                    .text_color(pal.text_muted),
            )
    }
}

#[derive(IntoElement)]
struct LabelTrigger {
    id: ElementId,
    label: Label,
    binding: BindingLabel,
    highlighted: bool,
    selected: bool,
    view: Entity<MouseModelView>,
}

impl RenderOnce for LabelTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let highlighted = self.highlighted || self.selected;
        let selected = self.selected;
        let btn = self.label.id;
        let view = self.view;
        let click_view = view.clone();
        let pal = theme::palette(cx);
        let binding_color = if highlighted {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_primary
        };
        // Always show the action the button actually performs. Default and
        // customised bindings use the same neutral value colour; only the
        // actively highlighted control takes the accent.
        let binding_description = self.binding.text.clone();
        let button_name = tr!(self.label.id.translation_key());
        BaseButton::new(self.id)
            .selected(selected)
            .accessibility_label(tr!("actions.bind_control", name => button_name.clone()))
            .aria_description(binding_description)
            .aria_selected(selected)
            .flex()
            .flex_col()
            .items_stretch()
            .w(px(LABEL_W))
            .h(px(LABEL_H))
            .px_3()
            .justify_center()
            .gap_0p5()
            .rounded(pal.control_radius)
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
            .focus_visible(move |s| {
                s.bg(if highlighted {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
                .border_color(rgb(ACCENT_BLUE))
            })
            // Button name — the caption (xs / muted), the same size as the
            // inspector title and category headers it shares the binding flow with.
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(button_name),
            )
            // Current binding — the value (sm), the same size as the action rows
            // it edits.
            .child(
                self.binding
                    .row(binding_color, pal)
                    .debug_selector(move || format!("label-value-row-{btn:?}")),
            )
            .on_click(move |_event, _window, cx| {
                click_view.update(cx, |this, cx| {
                    this.select(btn);
                    cx.notify();
                });
            })
            .on_hover(move |hovered, _window, cx| {
                set_control_hovered(&view, btn, *hovered, cx);
            })
    }
}

/// The label card's text and icon for one control.
pub(super) fn binding_label_for_control(
    control: MouseControlId,
    bindings: &std::collections::BTreeMap<ButtonId, Action>,
    gesture_buttons: &[ButtonId],
) -> BindingLabel {
    if control
        .button()
        .is_some_and(|button| gesture_buttons.contains(&button))
    {
        return BindingLabel {
            text: tr!("actions.five_directions"),
            icon: Some(GESTURE_BUTTON_ICON),
        };
    }

    match control {
        MouseControlId::Button(button) => {
            let action = bindings
                .get(&button)
                .cloned()
                .unwrap_or_else(|| default_binding(button));
            BindingLabel {
                text: localized_action_label(&action),
                icon: Some(action_icon_path(&action)),
            }
        }
        MouseControlId::ThumbwheelRotation => {
            let backward = bindings
                .get(&ButtonId::ThumbwheelScrollDown)
                .cloned()
                .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollDown));
            let forward = bindings
                .get(&ButtonId::ThumbwheelScrollUp)
                .cloned()
                .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollUp));
            if let Some(preset) = ThumbwheelPreset::recognize(&backward, &forward) {
                BindingLabel {
                    text: tr!(preset.translation_key()),
                    icon: Some(preset.icon()),
                }
            } else {
                BindingLabel {
                    text: tr!("common.custom"),
                    icon: Some("action-icons/chevrons-right.svg"),
                }
            }
        }
    }
}

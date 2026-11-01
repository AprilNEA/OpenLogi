//! The binding labels beside the mouse render: what each control card says and how it is placed in the side gutter.

use super::super::hotspots::MouseControlId;
use super::super::leader_lines::{Label, Side};
use super::super::thumbwheel::ThumbwheelPreset;
use crate::features::binding_editor::{GESTURE_BUTTON_ICON, action_icon_path};
use crate::ui::action::localized_action_label;
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};
use gpui::{
    App, ElementId, Entity, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    Refineable as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window,
    div, prelude::FluentBuilder as _, px, rems, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{Icon, IconName, h_flex};
use openlogi_core::binding::{Action, ButtonId, default_binding};

use super::{ModelRect, MouseModelView, SIDE_GAP, set_control_hovered};

/// The card's padding either side of its content, in rems.
const CARD_PAD_X: f32 = 0.75;
/// The value row's leading action icon, in rems.
const VALUE_ICON: f32 = 1.;
/// The gap between the value row's icon, text and chevron, in rems.
const VALUE_GAP: f32 = 0.5;
/// The value row's trailing chevron, in rems.
const CHEVRON: f32 = 0.75;
/// The card's `border_1` on either side, in pixels.
const CARD_BORDERS: f32 = 2.;
/// Layout snaps every box's edges to whole pixels, which can take up to a
/// pixel from the value's box, so a card sized to the exact text width could
/// still cut it off.
const PIXEL_SNAP: f32 = 1.;

/// The size every label card takes in one layout, and how many lines its
/// value may wrap to before it ends in an ellipsis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CardSize {
    pub(super) w: f32,
    pub(super) h: f32,
    pub(super) value_lines: usize,
}

/// The cards' values as this window renders them: the widest one on a
/// single line, the width a card adds around its value, and the height of
/// one more value line.
#[derive(Clone, Copy, Debug)]
pub(super) struct ValueMetrics {
    pub(super) widest: f32,
    pub(super) chrome_w: f32,
    pub(super) line_h: f32,
}

impl ValueMetrics {
    /// Measure `values` in the value row's text style, at this window's font
    /// and rem size.
    pub(super) fn measure(values: impl IntoIterator<Item = SharedString>, window: &Window) -> Self {
        let mut style = window.text_style();
        style.refine(div().text_body().text_style());
        let rem = window.rem_size();
        let font_size = style.font_size.to_pixels(rem);
        let line_w = |line: &str| {
            let shaped = window.text_system().shape_line(
                SharedString::from(line.to_owned()),
                font_size,
                &[style.to_run(line.len())],
                None,
            );
            f32::from(shaped.width)
        };
        let widest = values
            .into_iter()
            .map(|text| text.lines().map(line_w).fold(0., f32::max))
            .fold(0., f32::max);
        Self {
            widest,
            chrome_w: card_chrome_w(rem),
            line_h: f32::from(style.line_height_in_pixels(rem)),
        }
    }
}

/// The width a card adds around its value text: padding, borders, the icon,
/// the chevron, the gaps between them, and the pixel snapping may take.
fn card_chrome_w(rem: Pixels) -> f32 {
    f32::from(rems(2. * CARD_PAD_X + VALUE_ICON + 2. * VALUE_GAP + CHEVRON).to_pixels(rem))
        + CARD_BORDERS
        + PIXEL_SNAP
}

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
    let card = model.card;
    let x = match label.side {
        Side::Left => model.left - SIDE_GAP - card.w,
        Side::Right => model.left + model.width + SIDE_GAP,
    };
    let view = view.clone();
    let control = label.id;
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        value_lines: card.value_lines,
        highlighted,
        selected,
        view,
    };
    div()
        .absolute()
        .left(px(x))
        .top(px(label.y - card.h / 2.))
        .w(px(card.w))
        .h(px(card.h))
        .debug_selector(move || format!("label-card-{control:?}"))
        .child(trigger)
}

pub(super) struct BindingLabel {
    pub(super) text: SharedString,
    /// Vendored action-icon asset path (see [`action_icon_path`]) for the
    /// card's leading glyph. Every constructor currently supplies one; the
    /// `Option` is the seam for icon-less bindings.
    icon: Option<&'static str>,
}

impl BindingLabel {
    /// The card's value row: the leading action icon, the binding text, and
    /// the trailing chevron, all tinted with `color`. The text wraps to
    /// `value_lines` lines, the last ending in an ellipsis if it still does
    /// not fit.
    fn row(
        self,
        control: MouseControlId,
        color: Hsla,
        value_lines: usize,
        pal: theme::Palette,
    ) -> gpui::Div {
        h_flex()
            .items_center()
            .gap(rems(VALUE_GAP))
            // Leading action icon (same glyph as the picker rows), tinted with
            // the value so it tracks the default / set / highlighted state.
            .when_some(self.icon, |row, path| {
                row.child(
                    svg()
                        .path(path)
                        .size(rems(VALUE_ICON))
                        .flex_none()
                        .text_color(color),
                )
            })
            .child(
                // Shrink so a value too long for the card keeps the chevron
                // inside it.
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .when(value_lines == 1, Styled::whitespace_nowrap)
                    .line_clamp(value_lines)
                    .text_body()
                    .text_color(color)
                    .debug_selector(move || format!("label-value-text-{control:?}"))
                    .child(self.text),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(rems(CHEVRON))
                    .text_color(pal.text_muted),
            )
    }
}

#[derive(IntoElement)]
struct LabelTrigger {
    id: ElementId,
    label: Label,
    binding: BindingLabel,
    value_lines: usize,
    highlighted: bool,
    selected: bool,
    view: Entity<MouseModelView>,
}

impl RenderOnce for LabelTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let highlighted = self.highlighted || self.selected;
        let selected = self.selected;
        let btn = self.label.id;
        let value_lines = self.value_lines;
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
            .size_full()
            .px(rems(CARD_PAD_X))
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
                    .row(btn, binding_color, value_lines, pal)
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

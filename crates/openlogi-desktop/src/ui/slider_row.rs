//! The one layout every slider setting shares.
//!
//! A slider setting reads as one control: its label and live value on one
//! line at the body step, what it does in a caption under the label, the
//! slider across its container, and an optional caption under the slider for
//! its range and step or a note about it. The device pages and Settings had
//! drifted into three dialects — a muted label over a primary value, a caption
//! label over an accent value that read as a link, and a narrow slider with
//! the value beside it — so [`SliderRow`] is the single rendering of that row.
//! The slider is still a [`CommitSlider`](super::commit_slider::CommitSlider)
//! the caller owns and renders; the row only lays it out.

use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontFeatures, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _, px, rems,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{h_flex, v_flex};

use super::theme::{self, FocusRingStyle as _, Palette, PressableStyle as _, Typography as _};

/// Selects a row's value column in GPUI tests.
const VALUE_SELECTOR: &str = "slider-row-value";

/// A slider setting: label and value, description, slider, hint.
#[derive(IntoElement)]
pub(crate) struct SliderRow {
    label: SharedString,
    control: AnyElement,
    value: Option<SharedString>,
    widest: Vec<SharedString>,
    marker: Option<AnyElement>,
    /// The button the value becomes when it can be typed in exactly.
    entry: Option<BaseButton>,
    description: Option<SharedString>,
    hint: Option<AnyElement>,
    disabled: bool,
    compact: bool,
}

impl SliderRow {
    /// A row labelled `label` around `control`: the `Slider`; what stands in
    /// for it while it cannot be used, such as a status or retry line or a
    /// [`disabled_track`]; or, for a setting that reads like one, the control
    /// it is chosen with instead.
    pub(crate) fn new(label: impl Into<SharedString>, control: impl IntoElement) -> Self {
        Self {
            label: label.into(),
            control: control.into_any_element(),
            value: None,
            widest: Vec::new(),
            marker: None,
            entry: None,
            description: None,
            hint: None,
            disabled: false,
            compact: false,
        }
    }

    /// Show `value` at the end of the label line, right-aligned in tabular
    /// figures in a column as wide as the widest of `widest` — the value's
    /// longest renderings, usually its formatted maximum — so neither the
    /// number nor the label moves while the thumb is dragged.
    #[must_use]
    pub(crate) fn value(
        mut self,
        value: impl Into<SharedString>,
        widest: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        self.value = Some(value.into());
        self.widest = widest.into_iter().map(Into::into).collect();
        self
    }

    /// What sits just before the value, muted and at its size: a word such as
    /// "Default", shown for one value at a time, or a small control that
    /// decides where the value comes from, such as a camera's Auto chip.
    #[must_use]
    pub(crate) fn marker(mut self, marker: impl IntoElement) -> Self {
        self.marker = Some(marker.into_any_element());
        self
    }

    /// Let the value be typed in exactly, for a range too fine to hit with
    /// the thumb: the value becomes an outlined button, named `label` for
    /// assistive technology, that runs `on_click` to open the entry. It is
    /// outlined like a field rather than coloured like a link, since it opens
    /// an editor in the app.
    #[must_use]
    pub(crate) fn value_entry(
        mut self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.entry = Some(
            BaseButton::new(id)
                .accessibility_label(label)
                .on_click(on_click),
        );
        self
    }

    /// What the setting does, in a caption under the label.
    #[must_use]
    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// A caption under the slider: its range and step, or a note about it.
    #[must_use]
    pub(crate) fn hint(mut self, hint: impl IntoElement) -> Self {
        self.hint = Some(hint.into_any_element());
        self
    }

    /// Mute the label and value while the setting does not apply.
    #[must_use]
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set the label, value and description in the step gpui-component's
    /// `SettingItem` gives its title and description, so a row in a Settings
    /// `SettingGroup` matches the kit items beside it. The kit does not let
    /// its items take the house type scale, so the row follows the kit there;
    /// device pages keep the body step.
    #[must_use]
    pub(crate) fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

impl RenderOnce for SliderRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let text = if self.disabled {
            pal.text_muted
        } else {
            pal.text_primary
        };
        let compact = self.compact;
        let caption = |content: AnyElement| {
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(content)
        };
        let description = self.description.map(|description| {
            if compact {
                // The kit's description: its title's size, muted, at the
                // line height the page gives it.
                div()
                    .text_sm()
                    .text_color(pal.text_muted)
                    .child(description)
            } else {
                caption(description.into_any_element())
            }
        });
        let value = self.value.map(|value| {
            let number = number(value, self.widest);
            let number = match self.entry {
                Some(button) => entry_button(button, number, pal).into_any_element(),
                None => number.into_any_element(),
            };
            h_flex()
                .debug_selector(|| VALUE_SELECTOR.into())
                .flex_shrink_0()
                .gap_1p5()
                .children(
                    self.marker
                        .map(|marker| div().text_color(pal.text_muted).child(marker)),
                )
                .child(number)
        });
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .child(
                        // Top-aligned rather than on the baseline: a label that
                        // wraps keeps the value on its first line.
                        h_flex()
                            .items_start()
                            .gap_4()
                            .map(|line| {
                                if compact {
                                    // The kit's title: its `Label` at `text_sm`.
                                    line.text_sm().line_height(rems(1.25))
                                } else {
                                    line.text_body()
                                }
                            })
                            .text_color(text)
                            .child(div().flex_1().min_w_0().child(self.label))
                            .children(value),
                    )
                    .children(description),
            )
            .child(self.control)
            .children(self.hint.map(caption))
    }
}

/// The value, right-aligned in tabular figures over invisible zero-height
/// copies of the widest values, which hold it at their width without adding
/// a line.
fn number(value: SharedString, widest: Vec<SharedString>) -> Div {
    v_flex()
        .items_end()
        .whitespace_nowrap()
        .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
        .child(value)
        .children(
            widest
                .into_iter()
                .map(|text| div().h_0().overflow_hidden().invisible().child(text)),
        )
}

/// The value as the button that opens its exact entry: outlined like the
/// other in-app controls, in the value's own colour.
fn entry_button(button: BaseButton, number: Div, pal: Palette) -> BaseButton {
    button
        .px_1p5()
        .py_0p5()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
        .pressable_fill(false, pal)
        .focus_visible(move |style| style.focus_ring(pal))
        .child(number)
}

/// What a row shows in place of its slider while the setting does not apply:
/// the bare track, so the row keeps its shape.
pub(crate) fn disabled_track(pal: Palette) -> Div {
    div().w_full().h(px(6.)).rounded_full().bg(pal.border)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Bounds, Context, Modifiers, Pixels, Render, TestAppContext, VisualTestContext};

    use super::*;

    struct Row {
        value: &'static str,
        widest: &'static [&'static str],
        /// Counts the clicks on a value that can be typed in.
        entry: Option<Rc<Cell<u32>>>,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let entry = self.entry.clone();
            div().w(px(320.)).child(
                SliderRow::new("Speed", div().h(px(6.)))
                    .description("How fast the pointer moves.")
                    .value(self.value, self.widest.iter().copied())
                    .when_some(entry, |row, clicks| {
                        row.value_entry("speed-entry", "Set speed", move |_, _, _| {
                            clicks.set(clicks.get() + 1);
                        })
                    }),
            )
        }
    }

    fn value_bounds(row: Row, cx: &mut TestAppContext) -> (Bounds<Pixels>, &mut VisualTestContext) {
        let (_, cx) = cx.add_window_view(|_, _| row);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx
            .debug_bounds(VALUE_SELECTOR)
            .expect("the value column is drawn");
        (bounds, cx)
    }

    /// A short value keeps the column as wide as the widest one, and the
    /// copies that hold it there add no height, so dragging the thumb moves
    /// neither the label nor the slider — whether the value is plain text or
    /// a button that opens its exact entry.
    #[gpui::test]
    fn the_value_column_keeps_the_widest_values_width_on_one_line(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for entry in [None, Some(Rc::default())] {
            let (widest, _) = value_bounds(
                Row {
                    value: "100",
                    widest: &[],
                    entry: entry.clone(),
                },
                cx,
            );
            let (short, _) = value_bounds(
                Row {
                    value: "5",
                    widest: &["1", "100"],
                    entry,
                },
                cx,
            );

            assert!(widest.size.width > Pixels::ZERO);
            assert_eq!(short.size, widest.size);
            assert_eq!(short.right(), widest.right());
        }
    }

    /// A value that can be typed in opens its entry when clicked anywhere
    /// across the column, not only on its digits.
    #[gpui::test]
    fn a_click_on_a_typed_value_opens_its_entry(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let clicks = Rc::new(Cell::new(0));
        let (bounds, cx) = value_bounds(
            Row {
                value: "5",
                widest: &["100"],
                entry: Some(clicks.clone()),
            },
            cx,
        );

        cx.simulate_click(bounds.center(), Modifiers::default());
        assert_eq!(clicks.get(), 1);
        // The left edge is the widest value's, clear of the short one.
        cx.simulate_click(
            bounds.left_center() + gpui::point(px(3.), px(0.)),
            Modifiers::default(),
        );
        assert_eq!(clicks.get(), 2);
    }
}

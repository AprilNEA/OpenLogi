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
    AnyElement, App, Div, FontFeatures, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, Styled, Window, div, px,
};
use gpui_component::{h_flex, v_flex};

use super::theme::{self, Palette, Typography as _};

/// Selects a row's value column in GPUI tests.
const VALUE_SELECTOR: &str = "slider-row-value";

/// A slider setting: label and value, description, slider, hint.
#[derive(IntoElement)]
pub(crate) struct SliderRow {
    label: SharedString,
    control: AnyElement,
    value: Option<SharedString>,
    widest: Vec<SharedString>,
    marker: Option<SharedString>,
    description: Option<SharedString>,
    hint: Option<AnyElement>,
    disabled: bool,
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
            description: None,
            hint: None,
            disabled: false,
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

    /// A muted word just before the value, at its size, such as "Default".
    /// It sits beside the value rather than at a fixed place, so it suits a
    /// marker shown for one value at a time.
    #[must_use]
    pub(crate) fn marker(mut self, marker: impl Into<SharedString>) -> Self {
        self.marker = Some(marker.into());
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
}

impl RenderOnce for SliderRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let text = if self.disabled {
            pal.text_muted
        } else {
            pal.text_primary
        };
        let caption = |content: AnyElement| {
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(content)
        };
        let value = self.value.map(|value| {
            let marker = self
                .marker
                .map(|marker| div().text_color(pal.text_muted).child(marker));
            value_column(marker, value, self.widest)
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
                            .text_body()
                            .text_color(text)
                            .child(div().flex_1().min_w_0().child(self.label))
                            .children(value),
                    )
                    .children(
                        self.description
                            .map(|description| caption(description.into_any_element())),
                    ),
            )
            .child(self.control)
            .children(self.hint.map(caption))
    }
}

/// The value, after its marker, right-aligned over invisible zero-height
/// copies of the widest values, which hold the column at their width without
/// adding a line.
fn value_column(marker: Option<Div>, value: SharedString, widest: Vec<SharedString>) -> Div {
    v_flex()
        .debug_selector(|| VALUE_SELECTOR.into())
        .flex_shrink_0()
        .items_end()
        .whitespace_nowrap()
        .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
        .child(h_flex().gap_1p5().children(marker).child(value))
        .children(
            widest
                .into_iter()
                .map(|text| div().h_0().overflow_hidden().invisible().child(text)),
        )
}

/// What a row shows in place of its slider while the setting does not apply:
/// the bare track, so the row keeps its shape.
pub(crate) fn disabled_track(pal: Palette) -> Div {
    div().w_full().h(px(6.)).rounded_full().bg(pal.border)
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, Context, Pixels, Render, TestAppContext, VisualTestContext};

    use super::*;

    struct Row {
        value: &'static str,
        widest: &'static [&'static str],
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(320.)).child(
                SliderRow::new("Speed", div().h(px(6.)))
                    .description("How fast the pointer moves.")
                    .value(self.value, self.widest.iter().copied()),
            )
        }
    }

    fn value_bounds(row: Row, cx: &mut TestAppContext) -> Bounds<Pixels> {
        let (_, cx) = cx.add_window_view(|_, _| row);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        VisualTestContext::debug_bounds(cx, VALUE_SELECTOR).expect("the value column is drawn")
    }

    /// A short value keeps the column as wide as the widest one, and the
    /// copies that hold it there add no height, so dragging the thumb moves
    /// neither the label nor the slider.
    #[gpui::test]
    fn the_value_column_keeps_the_widest_values_width_on_one_line(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let widest = value_bounds(
            Row {
                value: "100",
                widest: &[],
            },
            cx,
        );
        let short = value_bounds(
            Row {
                value: "5",
                widest: &["1", "100"],
            },
            cx,
        );

        assert!(widest.size.width > Pixels::ZERO);
        assert_eq!(short.size, widest.size);
        assert_eq!(short.right(), widest.right());
    }
}

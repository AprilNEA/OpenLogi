//! Status and retry lines for lazily loaded device settings.
//!
//! The DPI and SmartShift panels both resolve their device state in the
//! background and surface the same handful of non-`Ready` states — "reading…",
//! "offline", "unsupported", and a failed read with a retry. This module is the
//! single rendering of those rows so they read identically across panels; only
//! the retry action differs, injected by the caller.

use gpui::{
    App, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::{Disableable, Sizable as _, button::Button, h_flex};

use crate::ui::theme::{self, Palette, Typography as _};

/// Minimum height for a status / retry row, so swapping a slider out for a
/// status message (or back) doesn't make the panel jump.
const ROW_H: f32 = 28.;

/// A muted, non-interactive status line — "Reading…", "offline", "unsupported".
/// The text is pre-localized by the caller (panels hold their own `tr!` keys).
pub fn status_line(text: impl Into<SharedString>, pal: Palette) -> gpui::Div {
    div()
        .h(px(ROW_H))
        .text_body()
        .text_color(pal.text_muted)
        .child(text.into())
}

/// The recovery command for a failed read or write: an outlined button, so it
/// reads as a command rather than as a link.
pub fn retry_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_retry: impl Fn(&mut App) + 'static,
) -> Button {
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .on_click(move |_event, _window, cx| on_retry(cx))
}

/// A failed read: what went wrong, then a "Try again" [`retry_button`].
///
/// `debug_selector` and `disabled` apply to the button.
pub fn retry_line(
    id: impl Into<ElementId>,
    message: impl Into<SharedString>,
    on_retry: impl Fn(&mut App) + 'static,
) -> RetryLine {
    RetryLine {
        message: message.into(),
        button: retry_button(id, tr!("common.try_again"), on_retry),
    }
}

/// A failed-read row built by [`retry_line`]: the message, then its retry
/// button.
#[derive(IntoElement)]
pub struct RetryLine {
    message: SharedString,
    button: Button,
}

impl Disableable for RetryLine {
    fn disabled(mut self, disabled: bool) -> Self {
        self.button = self.button.disabled(disabled);
        self
    }
}

impl InteractiveElement for RetryLine {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.button.interactivity()
    }
}

impl StatefulInteractiveElement for RetryLine {}

impl RenderOnce for RetryLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        h_flex()
            .min_h(px(ROW_H))
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_body()
                    .text_color(pal.text_primary)
                    .child(self.message),
            )
            .child(self.button)
    }
}

#[cfg(test)]
mod tests;

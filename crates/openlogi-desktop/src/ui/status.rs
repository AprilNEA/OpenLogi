//! Status and retry lines for lazily loaded device settings.
//!
//! The DPI and SmartShift panels both resolve their device state in the
//! background and surface the same handful of non-`Ready` states — "reading…",
//! "offline", "unsupported", and a clickable "retry". This module is the single
//! rendering of those rows so they read identically across panels; only the
//! retry action differs, injected by the caller.

use gpui::{App, ElementId, ParentElement, SharedString, Styled, div, px};
use gpui_component::button::{Button, ButtonVariants as _};

use crate::ui::theme::{Palette, Typography as _};

/// Fixed height for a status / retry row, so swapping a slider out for a status
/// message (or back) doesn't make the panel jump.
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

/// A clickable accent line that re-arms a failed read on click. `on_retry` runs
/// the panel's query retry — the only recovery
/// path when the gallery holds a single device, where re-selecting is a no-op.
pub fn retry_line(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    pal: Palette,
    on_retry: impl Fn(&mut App) + 'static,
) -> Button {
    Button::new(id)
        .text()
        .h(px(ROW_H))
        .text_body()
        .text_color(pal.text_primary)
        .label(text)
        .on_click(move |_event, _window, cx| on_retry(cx))
}

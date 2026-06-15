//! Shared status / retry lines for the lazily-loaded device-config panels.
//!
//! The DPI and SmartShift panels both resolve their device state in the
//! background and surface the same handful of non-`Ready` states — "reading…",
//! "offline", "unsupported", and a clickable "retry". This module is the single
//! rendering of those rows so they read identically across panels; only the
//! retry action differs, injected by the caller.

use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, px,
};

use crate::theme::{self, Palette};

/// Fixed height for a status / retry row, so swapping a slider out for a status
/// message (or back) doesn't make the panel jump.
const ROW_H: f32 = 28.;

/// A muted, non-interactive status line — "Reading…", "offline", "unsupported".
/// The text is pre-localized by the caller (panels hold their own `tr!` keys).
pub fn status_line(text: impl Into<SharedString>, pal: Palette) -> AnyElement {
    div()
        .h(px(ROW_H))
        .text_sm()
        .text_color(pal.text_muted)
        .child(text.into())
        .into_any_element()
}

/// A clickable accent line that re-arms a failed read on click. `on_retry` runs
/// the panel's retry (e.g. [`AppState::retry_active_dpi`]) — the only recovery
/// path when the carousel holds a single device, where re-selecting is a no-op.
///
/// [`AppState::retry_active_dpi`]: crate::state::AppState::retry_active_dpi
pub fn retry_line(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    pal: Palette,
    on_retry: impl Fn(&mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(ROW_H))
        .text_sm()
        .text_color(theme::accent())
        .hover(|s| s.text_color(pal.text_primary))
        .child(text.into())
        .on_click(move |_event, _window, cx| on_retry(cx))
        .into_any_element()
}

//! Native overlay stubs for platforms without extra window policy.

use super::CursorPlacement;

pub(super) const fn configure_application() {}

pub(super) fn configure_window(_window: &gpui::Window) {}

pub(crate) struct ClickAwayMonitor;

pub(super) fn watch_clicks_outside(
    _on_mouse_down: impl Fn() + 'static,
) -> Option<ClickAwayMonitor> {
    None
}

pub(super) const fn cursor_placement(_x: f64, _y: f64) -> Option<CursorPlacement> {
    None
}

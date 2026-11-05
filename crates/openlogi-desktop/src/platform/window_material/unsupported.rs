//! Other platforms retain the theme owner's standard GPUI background.

use gpui::{Window, WindowBackgroundAppearance};

/// Other platforms have no app-owned AppKit material.
pub(super) fn apply(_window: &Window, _material: WindowBackgroundAppearance) {}

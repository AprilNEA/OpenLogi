//! Opaque window policy on platforms without the macOS material backend.

use gpui::App;

/// Acrylic Glass is currently available only on macOS.
pub(crate) fn allows_glass() -> bool {
    false
}

/// Other platforms need no native display-preference subscription.
pub(crate) fn install(_cx: &mut App) {}

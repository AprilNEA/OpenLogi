//! Apply the theme owner's window policy and its native material together.

use gpui::{Window, WindowBackgroundAppearance};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "macos"))]
use unsupported as platform;

/// Apply a material selected by `ui::theme::glass` to one live window.
///
/// Transparent GPUI rendering exposes the app-owned macOS material. Opaque
/// rendering removes it. This avoids GPUI's private blur-layer modifications.
pub(crate) fn apply(window: &Window, material: WindowBackgroundAppearance) {
    window.set_background_appearance(material);
    platform::apply(window, material);
}

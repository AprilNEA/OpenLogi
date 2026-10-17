//! Camera preview and controls.

pub mod controls;
pub mod preview;

/// Fire the Camera consent prompt, then notify permission-dependent views once
/// it resolves (the dialog is answered outside the app, so nothing else emits).
#[cfg(target_os = "macos")]
pub(crate) fn request_camera_access(cx: &mut gpui::App) {
    crate::state::AppState::request_camera_access(cx);
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn request_camera_access(_cx: &mut gpui::App) {}

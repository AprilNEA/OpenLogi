//! Camera preview and controls.

pub mod controls;
pub mod preview;

/// Fire the Camera consent prompt, then repaint every window once it resolves (the dialog is answered outside the app, so nothing else triggers a render).
#[cfg(target_os = "macos")]
pub(crate) fn request_camera_access(cx: &mut gpui::App) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static POLL_ACTIVE: AtomicBool = AtomicBool::new(false);
    const TICK: Duration = Duration::from_millis(250);
    const TICKS_MAX: u32 = 2400; // 10 minutes

    openlogi_camera::request_camera_access();
    if POLL_ACTIVE.swap(true, Ordering::SeqCst) {
        return;
    }
    cx.spawn(async move |cx| {
        for _ in 0..TICKS_MAX {
            cx.background_executor().timer(TICK).await;
            if openlogi_camera::camera_authorization()
                != openlogi_camera::CameraAuthorization::Undetermined
            {
                break;
            }
        }
        POLL_ACTIVE.store(false, Ordering::SeqCst);
        cx.update(gpui::App::refresh_windows);
    })
    .detach();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn request_camera_access(_cx: &mut gpui::App) {}

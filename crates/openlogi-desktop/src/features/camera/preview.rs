//! Live camera preview, driven by the parent view's tab visibility.
//!
//! [`CameraPreview::set_target`] is the single lifecycle switch: the parent
//! ([`crate::app::AppView`]) calls it each render with the active camera's id
//! while the live-preview tab is showing, or `None` otherwise. Passing `None`
//! — leaving the tab, going home, or selecting another device — drops the
//! `AVCaptureSession`, so the LED goes off and the camera leaves zero CPU,
//! memory, and GPU texture behind. The camera is therefore active *only* while
//! you are looking at it.
//!
//! While permission is undetermined the placeholder is a click target that
//! fires the system consent prompt
//! ([`crate::features::camera::request_camera_access`]) — the prompt must
//! originate in-app because macOS only lists an app under Privacy → Camera
//! after it has requested access at least once. Once the grant lands, the
//! helper's typed permission event starts the deferred stream.
//!
//! While streaming it captures at 720p (Retina-sharp for the 480pt box),
//! rebuilds the GPU texture only when a new frame arrives, and repaints at the
//! camera's ~30 fps delivery rate.
//!
//! When the camera cannot be opened at all the placeholder says why rather than
//! waiting on a first frame that is never coming — most usefully when another
//! application already holds the device.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, RenderImage, SharedString,
    Styled, Subscription, Task, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::v_flex;
use image::{Frame as ImageFrame, RgbaImage};
use openlogi_camera::{CameraAuthorization, CameraStream, CaptureError, Frame};

use crate::state::{AppState, StateEvent};
use crate::ui::theme::{self, Palette, Typography as _};

const PREVIEW_W: f32 = 480.;
const PREVIEW_H: f32 = 270.; // 16:9

/// Live preview view. Holds the capture stream + its texture only while the
/// parent points it at a camera via [`Self::set_target`].
pub struct CameraPreview {
    stream: Option<CameraStream>,
    streaming_uid: Option<String>,
    current_image: Option<Arc<RenderImage>>,
    last_generation: u64,
    /// Frame-rate repaint pump; exists only while streaming (dropping it cancels it).
    repaint_task: Option<Task<()>>,
    /// Target is set but the stream isn't running because Camera permission
    /// wasn't granted yet; retried once access appears.
    awaiting_access: bool,
    /// Why the last [`Self::start_stream`] failed, so the placeholder can say
    /// so instead of sitting on "Starting preview…" forever.
    start_error: Option<CaptureError>,
    _permission_obs: Subscription,
}

impl CameraPreview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let permission_obs = cx.subscribe(
            &AppState::global(cx),
            |preview, _, event: &StateEvent, cx| {
                if !matches!(event, StateEvent::CameraPermissionChanged) {
                    return;
                }
                if preview.awaiting_access && openlogi_camera::camera_access_granted() {
                    preview.awaiting_access = false;
                    preview.start_stream(cx);
                }
                cx.notify();
            },
        );
        Self {
            stream: None,
            streaming_uid: None,
            current_image: None,
            last_generation: 0,
            repaint_task: None,
            awaiting_access: false,
            start_error: None,
            _permission_obs: permission_obs,
        }
    }

    /// Point the preview at `target` (a camera's unique id) or `None` to stop.
    /// The parent calls this every render from the active detail tab, so the
    /// camera runs only while its preview is on screen. Idempotent when the
    /// target is unchanged, except that a stream deferred on missing Camera
    /// permission starts as soon as access is granted.
    pub fn set_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if target == self.streaming_uid {
            if self.awaiting_access && openlogi_camera::camera_access_granted() {
                self.awaiting_access = false;
                self.start_stream(cx);
                cx.notify();
            }
            return;
        }
        // Stop the old stream first: drop the session (LED off), cancel the
        // repaint pump, and free the GPU texture immediately — not in `render`,
        // which stops running the moment the preview leaves the screen.
        self.stream = None;
        self.repaint_task = None;
        self.last_generation = 0;
        self.awaiting_access = false;
        self.start_error = None;
        if let Some(old) = self.current_image.take() {
            cx.drop_image(old, None);
        }
        self.streaming_uid = target;

        if self.streaming_uid.is_none() {
            cx.notify();
            return;
        }
        // Only open the camera when access is already granted, so selecting it
        // never blocks the UI thread on the permission dialog.
        if openlogi_camera::camera_access_granted() {
            self.start_stream(cx);
        } else {
            self.awaiting_access = true;
        }
        cx.notify();
    }

    fn start_stream(&mut self, cx: &mut Context<Self>) {
        let Some(uid) = self.streaming_uid.as_deref() else {
            return;
        };
        match openlogi_camera::start_stream(uid) {
            Ok(stream) => {
                self.stream = Some(stream);
                self.start_error = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "camera preview failed to start");
                self.stream = None;
                self.start_error = Some(e);
            }
        }
        if self.stream.is_some() {
            self.repaint_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    // Repaint only when a *new* frame has arrived, so gpui isn't
                    // re-rendering the window on idle ticks.
                    let result = this.update(cx, |view, cx| {
                        let has_new = view
                            .stream
                            .as_ref()
                            .is_some_and(|s| s.frame_generation() != view.last_generation);
                        if has_new {
                            cx.notify();
                        }
                    });
                    if result.is_err() {
                        break;
                    }
                }
            }));
        }
    }
}

impl Render for CameraPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let granted = openlogi_camera::camera_access_granted();

        // Rebuild the texture only when a new frame arrived; free the old one.
        if let Some(stream) = self.stream.as_ref() {
            let generation = stream.frame_generation();
            if generation != self.last_generation
                && let Some(image) = stream
                    .take_frame()
                    .and_then(|f| build_image(Arc::unwrap_or_clone(f)))
            {
                if let Some(old) = self.current_image.take() {
                    let _ = window.drop_image(old);
                }
                self.current_image = Some(image);
                self.last_generation = generation;
            }
        }

        let image = self.current_image.clone();
        let show_placeholder = image.is_none();
        let capture_supported = !show_placeholder || openlogi_camera::capture_supported();
        let authorization_undetermined = show_placeholder
            && capture_supported
            && !granted
            && matches!(
                openlogi_camera::camera_authorization(),
                CameraAuthorization::Undetermined
            );

        v_flex()
            .w(px(PREVIEW_W))
            .h(px(PREVIEW_H))
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .when_some(image, |surface, image| {
                surface.child(img(image).w(px(PREVIEW_W)).h(px(PREVIEW_H)).rounded_md())
            })
            .when(show_placeholder && !capture_supported, |surface| {
                surface.child(note(tr!("camera.camera_preview_platform_unavailable"), pal))
            })
            .when(
                show_placeholder && capture_supported && granted,
                |surface| {
                    surface.child(note(
                        self.start_error
                            .as_ref()
                            .map_or_else(|| tr!("camera.starting_preview"), failure_note),
                        pal,
                    ))
                },
            )
            .when(
                show_placeholder && capture_supported && !granted && authorization_undetermined,
                |surface| {
                    surface.child(
                        BaseButton::new("camera-request-access")
                            .accessibility_label(tr!("camera.click_to_enable_camera_access"))
                            .text_body()
                            .text_color(pal.text_muted)
                            .cursor_pointer()
                            .hover(|s| s.text_color(pal.text_primary))
                            .focus_visible(|s| s.text_color(pal.text_primary))
                            .child(tr!("camera.click_to_enable_camera_access"))
                            .on_click(|_, _, cx| {
                                crate::features::camera::request_camera_access(cx);
                            }),
                    )
                },
            )
            .when(
                show_placeholder && capture_supported && !granted && !authorization_undetermined,
                |surface| {
                    surface.child(note(tr!("camera.camera_preview_permission_required"), pal))
                },
            )
    }
}

/// Wrap a BGRA camera frame as a gpui texture. The frame is already in gpui's
/// BGRA order and is consumed whole, so no pixel buffer is copied or swapped.
fn build_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![ImageFrame::new(buffer)])))
}

/// What the placeholder says when the camera could not be opened. Only the
/// in-use case gets its own wording: it is the one failure the user can act on
/// (close the other application), and the one that used to render as an
/// indefinite "Starting preview…" over a black box.
fn failure_note(error: &CaptureError) -> SharedString {
    match error {
        CaptureError::InUse => tr!("camera.camera_in_use_by_another_app"),
        _ => tr!("camera.camera_preview_start_failed"),
    }
}

fn note(text: impl Into<SharedString>, pal: Palette) -> gpui::Div {
    div()
        .text_body()
        .text_color(pal.text_muted)
        .child(text.into())
}

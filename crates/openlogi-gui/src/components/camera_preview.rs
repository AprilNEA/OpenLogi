//! Live webcam preview, driven by the parent view's tab visibility.
//!
//! [`CameraPreview::set_target`] is the single lifecycle switch: the parent
//! ([`crate::app::AppView`]) calls it each render with the active camera's id
//! while the live-preview tab is showing, or `None` otherwise. Passing `None`
//! — leaving the tab, going home, or selecting another device — drops the
//! `AVCaptureSession`, so the LED goes off and the camera leaves zero CPU,
//! memory, and GPU texture behind. The camera is therefore active *only* while
//! you are looking at it.
//!
//! While streaming it captures at 720p (Retina-sharp for the 480pt box),
//! rebuilds the GPU texture only when a new frame arrives, and repaints at the
//! camera's ~30 fps delivery rate.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, RenderImage, SharedString, Styled,
    Task, Window, div, img, px,
};
use gpui_component::v_flex;
use image::{Frame as ImageFrame, RgbaImage};
use openlogi_camera::{CameraStream, Frame};

use crate::theme::{self, Palette};

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
}

impl CameraPreview {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            stream: None,
            streaming_uid: None,
            current_image: None,
            last_generation: 0,
            repaint_task: None,
        }
    }

    /// Point the preview at `target` (a camera's unique id) or `None` to stop.
    /// The parent calls this every render from the active detail tab, so the
    /// camera runs only while its preview is on screen. Idempotent when the
    /// target is unchanged.
    pub fn set_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if target == self.streaming_uid {
            return;
        }
        // Stop the old stream first: drop the session (LED off), cancel the
        // repaint pump, and free the GPU texture immediately — not in `render`,
        // which stops running the moment the preview leaves the screen.
        self.stream = None;
        self.repaint_task = None;
        self.last_generation = 0;
        if let Some(old) = self.current_image.take() {
            cx.drop_image(old, None);
        }
        self.streaming_uid.clone_from(&target);

        let Some(uid) = target else {
            cx.notify();
            return;
        };
        // Only open the camera when access is already granted, so selecting it
        // never blocks the UI thread on the permission dialog.
        if openlogi_camera::camera_access_granted() {
            self.stream = openlogi_camera::start_stream(&uid).ok();
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
        cx.notify();
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

        let surface: AnyElement = if let Some(image) = self.current_image.as_ref() {
            img(image.clone())
                .w(px(PREVIEW_W))
                .h(px(PREVIEW_H))
                .rounded_md()
                .into_any_element()
        } else if !openlogi_camera::capture_supported() {
            note(
                tr!("Live preview isn't available on this platform yet."),
                pal,
            )
        } else if granted {
            note(tr!("Starting preview…"), pal)
        } else {
            note(tr!("Enable Camera access in Settings to preview."), pal)
        };

        v_flex()
            .w(px(PREVIEW_W))
            .h(px(PREVIEW_H))
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.surface)
            .child(surface)
    }
}

/// Wrap a BGRA camera frame as a gpui texture. The frame is already in gpui's
/// BGRA order and is consumed whole, so no pixel buffer is copied or swapped.
fn build_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![ImageFrame::new(buffer)])))
}

fn note(text: impl Into<SharedString>, pal: Palette) -> AnyElement {
    div()
        .text_sm()
        .text_color(pal.text_muted)
        .child(text.into())
        .into_any_element()
}

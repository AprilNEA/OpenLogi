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
//! While streaming it captures at the resolution and frame rate chosen in the
//! rows under the preview (720p at about 30 fps until one is chosen; the rows
//! appear where the platform backend lists modes), rebuilds the GPU texture
//! only when a new frame arrives, and repaints at the camera's delivery rate.
//! The choice only shapes this preview's own stream — it is saved per camera
//! ([`AppState::commit_camera_stream_mode`]) and is not a device setting other
//! applications see. A changed choice reopens the stream: the old one is
//! dropped, the last frame stays on screen, and the new one opens after the
//! camera has had [`openlogi_camera::RELEASE_DELAY`] to let go, on a timer so
//! the UI thread never waits on the driver.
//!
//! When the camera cannot be opened at all the placeholder says why rather than
//! waiting on a first frame that is never coming. Resource contention can clear
//! without an event, so the preview retries on a timer while its tab is on screen.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, Rems, Render,
    RenderImage, SharedString, Styled, Subscription, Task, Window, div, img,
    prelude::FluentBuilder as _, rems,
};
use gpui_base::Button as BaseButton;
use gpui_component::v_flex;
use image::{Frame as ImageFrame, RgbaImage};
use openlogi_camera::{CameraAuthorization, CaptureError, Frame, StreamMode, StreamModes};

use crate::state::{AppState, StateEvent};
use crate::ui::theme::{self, Palette, Typography as _};

mod capture;
use capture::{Capture, PreviewStream, SystemCapture};

mod modes;

#[cfg(test)]
mod tests;

// In rems, like the card that frames it, so the interface scale sizes both:
// a fixed-pixel preview overflows the card below 100 % and leaves a gap above.
pub(crate) const PREVIEW_W: Rems = rems(30.); // 480 px at the standard 16 px rem
const PREVIEW_H: Rems = rems(16.875); // 16:9
/// How long to wait before opening a camera again after a failed start. A
/// camera another application holds becomes free when that application lets go
/// of it, which raises no event to wait on — so poll, slowly enough that a
/// camera left busy costs one activation attempt every couple of seconds.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Live preview view. Holds the capture stream + its texture only while the
/// parent points it at a camera via [`Self::set_target`].
pub struct CameraPreview {
    capture: Box<dyn Capture>,
    lifecycle: PreviewLifecycle,
    /// What the selected camera offers, read each time its stream (re)opens.
    /// Empty before the first open, after the target changes, and on backends
    /// that list nothing — which hides the mode rows.
    modes: StreamModes,
    current_image: Option<Arc<RenderImage>>,
    _state_obs: Subscription,
}

enum PreviewLifecycle {
    Stopped,
    /// A target remains selected after opening its stream failed. Same-target
    /// renders stay idempotent rather than retrying the open in a hot loop.
    StartFailed {
        target: String,
        error: CaptureError,
        /// Leaving this state cancels its pending retry.
        _retry_task: Task<()>,
    },
    /// A target is selected but waits for Camera permission before opening.
    AwaitingAccess(String),
    Streaming {
        target: String,
        stream: Box<dyn PreviewStream>,
        /// The mode the stream was opened at: the saved choice if the camera
        /// offers it, else its default. `None` where no modes are listed. It is
        /// compared with the *resolved* saved choice, never the raw one, so
        /// choosing the mode the camera already defaults to doesn't reopen it.
        mode: Option<StreamMode>,
        last_generation: u64,
        /// Dropping the streaming state cancels its frame-rate repaint pump.
        _repaint_task: Task<()>,
    },
    /// The stream was dropped to change mode and the camera is being given
    /// [`openlogi_camera::RELEASE_DELAY`] to let go before it reopens. The last
    /// frame stays on screen meanwhile. Leaving this state cancels the reopen.
    Restarting {
        target: String,
        _reopen_task: Task<()>,
    },
}

impl PreviewLifecycle {
    fn target(&self) -> Option<&str> {
        match self {
            Self::Stopped => None,
            Self::StartFailed { target, .. }
            | Self::AwaitingAccess(target)
            | Self::Streaming { target, .. }
            | Self::Restarting { target, .. } => Some(target),
        }
    }
}

/// The camera `target` is, as the selected device: its config key and the mode
/// saved for it (`None` when none is chosen). `None` when `target` is not the
/// selected camera — the preview then falls back to the backend's default and
/// shows no mode rows, rather than guessing whose settings to use.
fn saved_mode(target: &str, cx: &App) -> Option<(String, Option<StreamMode>)> {
    let state = AppState::try_read(cx)?;
    let record = state
        .current_record()
        .filter(|record| record.capture_id.as_deref() == Some(target))?;
    Some((
        record.config_key.clone(),
        state.camera_stream_mode(&record.config_key),
    ))
}

impl CameraPreview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_capture(Box::new(SystemCapture), cx)
    }

    fn with_capture(capture: Box<dyn Capture>, cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(
            &AppState::global(cx),
            |preview, _, event: &StateEvent, cx| match event {
                StateEvent::CameraPermissionChanged => {
                    if preview.capture.access_granted() {
                        preview.start_deferred_stream(cx);
                    }
                    cx.notify();
                }
                // The camera's saved mode may have changed: reopen at it, and
                // repaint so the rows mark the new choice. The root view does
                // not repaint on this event while the Camera tab is open, so
                // the preview listens for itself.
                StateEvent::CameraChanged => {
                    preview.apply_saved_mode(cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        Self {
            capture,
            lifecycle: PreviewLifecycle::Stopped,
            modes: StreamModes::default(),
            current_image: None,
            _state_obs: state_obs,
        }
    }

    /// Point the preview at `target` (a camera's unique id) or `None` to stop.
    /// The parent calls this every render from the active detail tab, so the
    /// camera runs only while its preview is on screen. Idempotent when the
    /// target is unchanged, except that a stream deferred on missing Camera
    /// permission starts as soon as access is granted.
    pub fn set_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if target.as_deref() == self.lifecycle.target() {
            if self.capture.access_granted() && self.start_deferred_stream(cx) {
                cx.notify();
            }
            return;
        }
        // Stop the old stream first: drop the session (LED off), cancel the
        // repaint pump, and free the GPU texture immediately — not in `render`,
        // which stops running the moment the preview leaves the screen.
        self.lifecycle = PreviewLifecycle::Stopped;
        // Another camera's modes must not be offered for this one.
        self.modes = StreamModes::default();
        if let Some(old) = self.current_image.take() {
            cx.drop_image(old, None);
        }

        let Some(target) = target else {
            cx.notify();
            return;
        };
        self.open_when_granted(target, cx);
        cx.notify();
    }

    /// Open `target`, unless Camera access is not granted: then wait for it.
    /// Opening only on a standing grant means selecting a camera never blocks
    /// the UI thread on the permission dialog.
    fn open_when_granted(&mut self, target: String, cx: &mut Context<Self>) {
        if self.capture.access_granted() {
            self.start_stream(target, cx);
        } else {
            self.lifecycle = PreviewLifecycle::AwaitingAccess(target);
        }
    }

    /// Reopen the stream if the saved mode now resolves to a different one than
    /// the stream runs at. Anything but a running stream picks the saved mode
    /// up when it next opens.
    fn apply_saved_mode(&mut self, cx: &mut Context<Self>) {
        let PreviewLifecycle::Streaming {
            target,
            mode: running,
            ..
        } = &self.lifecycle
        else {
            return;
        };
        let Some((_, saved)) = saved_mode(target, cx) else {
            return;
        };
        if self.modes.resolve(saved) == *running {
            return;
        }
        let target = target.clone();
        self.restart(target, cx);
    }

    /// Drop the stream and reopen `target` once the camera has let go of it.
    ///
    /// The wait is a timer, not a blocking retry: reopening at once would make
    /// `start_stream` stall the UI thread on the driver's teardown
    /// ([`openlogi_camera::RELEASE_DELAY`]). The saved mode is read when the
    /// timer fires, so a second pick during the wait lands too.
    fn restart(&mut self, target: String, cx: &mut Context<Self>) {
        let reopen_target = target.clone();
        let delay = self.capture.release_delay();
        let reopen_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |view, cx| {
                if !matches!(
                    &view.lifecycle,
                    PreviewLifecycle::Restarting { target, .. } if *target == reopen_target
                ) {
                    return;
                }
                view.open_when_granted(reopen_target, cx);
                cx.notify();
            });
        });
        // Replacing `Streaming` drops the stream (LED off, camera released) and
        // cancels its repaint pump. The texture stays: a frozen last frame
        // reads better than a blank box for under a second.
        self.lifecycle = PreviewLifecycle::Restarting {
            target,
            _reopen_task: reopen_task,
        };
        cx.notify();
    }

    fn start_deferred_stream(&mut self, cx: &mut Context<Self>) -> bool {
        let lifecycle = std::mem::replace(&mut self.lifecycle, PreviewLifecycle::Stopped);
        let PreviewLifecycle::AwaitingAccess(target) = lifecycle else {
            self.lifecycle = lifecycle;
            return false;
        };
        self.start_stream(target, cx);
        true
    }

    fn start_stream(&mut self, target: String, cx: &mut Context<Self>) {
        // What the camera offers, and the saved choice among it. Driver
        // metadata only, so cheap enough for the UI thread. A camera whose
        // modes cannot be read gets none listed and the backend's default;
        // opening it below then reports the real error.
        self.modes = self.capture.stream_modes(&target).unwrap_or_else(|error| {
            tracing::debug!(%error, "camera modes unavailable");
            StreamModes::default()
        });
        let saved = saved_mode(&target, cx).and_then(|(_, saved)| saved);
        let mode = self.modes.resolve(saved);

        let stream = match self.capture.start_stream(&target, mode) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "camera preview failed to start");
                let retry_target = target.clone();
                let retry_task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RETRY_INTERVAL).await;
                    let _ = this.update(cx, |view, cx| {
                        if !matches!(
                            &view.lifecycle,
                            PreviewLifecycle::StartFailed { target, .. } if *target == retry_target
                        ) {
                            return;
                        }
                        view.open_when_granted(retry_target, cx);
                        cx.notify();
                    });
                });
                self.lifecycle = PreviewLifecycle::StartFailed {
                    target,
                    error,
                    _retry_task: retry_task,
                };
                return;
            }
        };
        let repaint_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                // Repaint only when a *new* frame has arrived, so gpui isn't
                // re-rendering the window on idle ticks.
                let result = this.update(cx, |view, cx| {
                    let has_new = match &view.lifecycle {
                        PreviewLifecycle::Streaming {
                            stream,
                            last_generation,
                            ..
                        } => stream.frame_generation() != *last_generation,
                        PreviewLifecycle::Stopped
                        | PreviewLifecycle::StartFailed { .. }
                        | PreviewLifecycle::AwaitingAccess(_)
                        | PreviewLifecycle::Restarting { .. } => false,
                    };
                    if has_new {
                        cx.notify();
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        });
        self.lifecycle = PreviewLifecycle::Streaming {
            target,
            stream,
            mode,
            last_generation: 0,
            _repaint_task: repaint_task,
        };
    }
}

impl Render for CameraPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let granted = self.capture.access_granted();

        // Rebuild the texture only when a new frame arrived; free the old one.
        if let PreviewLifecycle::Streaming {
            stream,
            last_generation,
            ..
        } = &mut self.lifecycle
        {
            let generation = stream.frame_generation();
            if generation != *last_generation
                && let Some(image) = stream
                    .take_frame()
                    .and_then(|f| build_image(Arc::unwrap_or_clone(f)))
            {
                if let Some(old) = self.current_image.take() {
                    let _ = window.drop_image(old);
                }
                self.current_image = Some(image);
                *last_generation = generation;
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
        let mode_rows = self.mode_rows(pal, cx);

        let surface = v_flex()
            .w(PREVIEW_W)
            .h(PREVIEW_H)
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .when_some(image, |surface, image| {
                surface.child(img(image).w(PREVIEW_W).h(PREVIEW_H).rounded_md())
            })
            .when(show_placeholder && !capture_supported, |surface| {
                surface.child(note(tr!("camera.camera_preview_platform_unavailable"), pal))
            })
            .when(
                show_placeholder && capture_supported && granted,
                |surface| {
                    surface.child(note(
                        match &self.lifecycle {
                            PreviewLifecycle::StartFailed {
                                error: CaptureError::ResourcesUnavailable,
                                ..
                            } => tr!("camera.camera_resources_unavailable"),
                            PreviewLifecycle::StartFailed {
                                error: CaptureError::AccessDenied,
                                ..
                            } => tr!("camera.camera_preview_permission_required"),
                            PreviewLifecycle::StartFailed { .. } => {
                                tr!("camera.camera_preview_start_failed")
                            }
                            _ => tr!("camera.starting_preview"),
                        },
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
            );

        v_flex()
            .gap_3()
            .child(surface)
            .when_some(mode_rows, ParentElement::child)
    }
}

impl CameraPreview {
    /// The resolution and frame-rate rows, when the selected camera lists modes.
    ///
    /// Shown whenever its modes are known — streaming, restarting, or a start
    /// that failed — because a mode that will not open is exactly when picking
    /// another is wanted. The marked pill is the mode the stream resolves to,
    /// not the raw saved choice, so a stale choice shows the default it fell
    /// back to.
    fn mode_rows(&self, pal: Palette, cx: &App) -> Option<AnyElement> {
        if self.modes.is_empty() {
            return None;
        }
        let (config_key, saved) = saved_mode(self.lifecycle.target()?, cx)?;
        let selected = self.modes.resolve(saved)?;
        Some(modes::mode_section(&config_key, &self.modes, selected, pal))
    }
}

/// Wrap a BGRA camera frame as a gpui texture. The frame is already in gpui's
/// BGRA order and is consumed whole, so no pixel buffer is copied or swapped.
fn build_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![ImageFrame::new(buffer)])))
}

fn note(text: impl Into<SharedString>, pal: Palette) -> gpui::Div {
    div()
        .max_w_full()
        .px_4()
        .text_center()
        .text_body()
        .text_color(pal.text_muted)
        .child(text.into())
}

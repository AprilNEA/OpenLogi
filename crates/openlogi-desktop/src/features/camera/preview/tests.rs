use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, point, px, size};
use openlogi_camera::{Camera, CaptureError};
use openlogi_core::config::Config;

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::Sources;

#[derive(Default)]
struct FakeCapture {
    granted: Cell<bool>,
    failure: RefCell<Option<CaptureError>>,
    /// What every camera reports offering. Empty, like a backend that lists none.
    modes: RefCell<StreamModes>,
    /// The mode each successful or failed open asked for, in order.
    opened_modes: RefCell<Vec<Option<StreamMode>>>,
    events: Rc<RefCell<Vec<String>>>,
    frames: RefCell<Vec<Rc<Frames>>>,
}

impl Capture for Rc<FakeCapture> {
    fn access_granted(&self) -> bool {
        self.granted.get()
    }

    fn release_delay(&self) -> Duration {
        RELEASE_DELAY
    }

    fn stream_modes(&self, _target: &str) -> Result<StreamModes, CaptureError> {
        Ok(self.modes.borrow().clone())
    }

    fn start_stream(
        &self,
        target: &str,
        mode: Option<StreamMode>,
    ) -> Result<Box<dyn PreviewStream>, CaptureError> {
        self.opened_modes.borrow_mut().push(mode);
        self.events.borrow_mut().push(format!("open {target}"));
        if let Some(error) = self.failure.borrow().clone() {
            return Err(error);
        }
        let frames = Rc::new(Frames::default());
        self.frames.borrow_mut().push(frames.clone());
        Ok(Box::new(FakeStream {
            frames,
            target: target.to_owned(),
            events: self.events.clone(),
        }))
    }
}

#[derive(Default)]
struct Frames {
    latest: RefCell<Option<Arc<Frame>>>,
    generation: Cell<u64>,
    polls: Cell<usize>,
    takes: Cell<usize>,
}

impl Frames {
    fn publish(&self, bgra: Vec<u8>) {
        *self.latest.borrow_mut() = Some(Arc::new(Frame {
            width: 2,
            height: 1,
            bgra,
        }));
        self.generation.set(self.generation.get() + 1);
    }
}

struct FakeStream {
    frames: Rc<Frames>,
    target: String,
    events: Rc<RefCell<Vec<String>>>,
}

impl PreviewStream for FakeStream {
    fn frame_generation(&self) -> u64 {
        self.frames.polls.set(self.frames.polls.get() + 1);
        self.frames.generation.get()
    }

    fn take_frame(&self) -> Option<Arc<Frame>> {
        self.frames.takes.set(self.frames.takes.get() + 1);
        self.frames.latest.borrow_mut().take()
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.events
            .borrow_mut()
            .push(format!("drop {}", self.target));
    }
}

fn preview(cx: &mut TestAppContext) -> (Entity<CameraPreview>, Rc<FakeCapture>) {
    cx.update(gpui_component::init);
    cx.update(|cx| {
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state =
            cx.new(|_| AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands)));
        AppState::set_global(state, cx);
    });
    let capture = Rc::new(FakeCapture::default());
    let view = cx.new(|cx| CameraPreview::with_capture(Box::new(capture.clone()), cx));
    (view, capture)
}

fn permission_event(cx: &mut TestAppContext) {
    cx.update(|cx| {
        AppState::global(cx).update(cx, |_, cx| cx.emit(StateEvent::CameraPermissionChanged));
    });
}

fn tick(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
}

fn draw(view: &Entity<CameraPreview>, cx: &mut VisualTestContext) {
    cx.draw(
        point(px(0.), px(0.)),
        size(PREVIEW_W.to_pixels(px(16.)), PREVIEW_H.to_pixels(px(16.))),
        |_, _| view.clone().into_any_element(),
    );
}

#[gpui::test]
fn permission_event_starts_the_waiting_target_exactly_once(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    view.update(cx, |view, cx| {
        view.set_target(Some("a".into()), cx);
        view.set_target(Some("a".into()), cx);
        assert!(matches!(&view.lifecycle, PreviewLifecycle::AwaitingAccess(id) if id == "a"));
    });
    permission_event(cx);
    assert!(capture.events.borrow().is_empty());

    capture.granted.set(true);
    permission_event(cx);
    assert_eq!(
        *capture.events.borrow(),
        ["open a"],
        "the event must start capture before another render"
    );
    permission_event(cx);
    view.update(cx, |view, cx| {
        view.set_target(Some("a".into()), cx);
        assert!(
            matches!(&view.lifecycle, PreviewLifecycle::Streaming { target, .. } if target == "a")
        );
    });
    assert_eq!(*capture.events.borrow(), ["open a"]);
}

#[gpui::test]
fn permission_on_render_starts_only_the_current_waiting_target(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    view.update(cx, |view, cx| {
        view.set_target(Some("superseded".into()), cx);
        view.set_target(Some("current".into()), cx);
    });
    capture.granted.set(true);
    view.update(cx, |view, cx| {
        view.set_target(Some("current".into()), cx);
        view.set_target(Some("current".into()), cx);
    });
    permission_event(cx);
    assert_eq!(*capture.events.borrow(), ["open current"]);
}

#[gpui::test]
fn stopping_while_waiting_does_not_open_after_a_late_grant(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    view.update(cx, |view, cx| {
        view.set_target(Some("a".into()), cx);
        view.set_target(None, cx);
    });
    capture.granted.set(true);
    permission_event(cx);
    view.update(cx, |view, cx| {
        view.set_target(None, cx);
        assert!(matches!(view.lifecycle, PreviewLifecycle::Stopped));
    });
    assert!(capture.events.borrow().is_empty());
}

#[gpui::test]
fn failed_start_does_not_retry_on_same_target_renders_or_permission_events(
    cx: &mut TestAppContext,
) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    *capture.failure.borrow_mut() = Some(CaptureError::Setup("test failure".into()));
    view.update(cx, |view, cx| {
        view.set_target(Some("a".into()), cx);
        assert!(
            matches!(&view.lifecycle, PreviewLifecycle::StartFailed { target, .. } if target == "a")
        );
        for _ in 0..3 {
            view.set_target(Some("a".into()), cx);
        }
    });
    permission_event(cx);
    assert_eq!(*capture.events.borrow(), ["open a"]);

    // Reselecting after leaving is a new attempt. This contract says nothing
    // about a future explicit retry action/timer: repeated renders aren't one.
    *capture.failure.borrow_mut() = None;
    view.update(cx, |view, cx| {
        view.set_target(None, cx);
        view.set_target(Some("a".into()), cx);
        assert!(matches!(view.lifecycle, PreviewLifecycle::Streaming { .. }));
    });
    assert_eq!(*capture.events.borrow(), ["open a", "open a"]);
}

#[gpui::test]
fn retries_wait_two_seconds_update_the_error_and_stop_after_success(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    *capture.failure.borrow_mut() = Some(CaptureError::ResourcesUnavailable);
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    let notifies = Rc::new(Cell::new(0));
    let _observer = cx.update(|cx| {
        cx.observe(&view, {
            let notifies = notifies.clone();
            move |_, _| notifies.set(notifies.get() + 1)
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(1999));
    cx.run_until_parked();
    assert_eq!(*capture.events.borrow(), ["open a"]);
    assert_eq!(notifies.get(), 0);

    *capture.failure.borrow_mut() = Some(CaptureError::AccessDenied);
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(*capture.events.borrow(), ["open a", "open a"]);
    assert_eq!(notifies.get(), 1, "a changed failure must repaint the note");
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::StartFailed { target, error: CaptureError::AccessDenied, .. }
                if target == "a"
        ));
    });

    *capture.failure.borrow_mut() = None;
    cx.executor().advance_clock(Duration::from_millis(1999));
    cx.run_until_parked();
    assert_eq!(capture.events.borrow().len(), 2);
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(*capture.events.borrow(), ["open a", "open a", "open a"]);
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming { target, .. } if target == "a"
        ));
    });
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();
    assert_eq!(capture.events.borrow().len(), 3, "success cancels retries");
}

#[gpui::test]
fn retry_deadlines_belong_to_the_selected_target_and_cancel_on_stop_or_drop(
    cx: &mut TestAppContext,
) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    *capture.failure.borrow_mut() = Some(CaptureError::Setup("test failure".into()));
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    view.update(cx, |view, cx| view.set_target(Some("b".into()), cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(
        *capture.events.borrow(),
        ["open a", "open b"],
        "a's old deadline must neither reopen a nor accelerate b"
    );
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(*capture.events.borrow(), ["open a", "open b", "open b"]);

    view.update(cx, |view, cx| view.set_target(None, cx));
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(capture.events.borrow().len(), 3);
    view.update(cx, |view, cx| view.set_target(Some("c".into()), cx));
    cx.run_until_parked();
    let weak = view.downgrade();
    drop(view);
    cx.update(|_| {});
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert!(weak.upgrade().is_none(), "retry must not retain the view");
    assert_eq!(
        *capture.events.borrow(),
        ["open a", "open b", "open b", "open c"]
    );
}

#[gpui::test]
fn a_retry_waits_for_revoked_access_before_opening_again(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    *capture.failure.borrow_mut() = Some(CaptureError::ResourcesUnavailable);
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    cx.run_until_parked();
    capture.granted.set(false);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(*capture.events.borrow(), ["open a"]);
    view.read_with(cx, |view, _| {
        assert!(
            matches!(&view.lifecycle, PreviewLifecycle::AwaitingAccess(target) if target == "a")
        );
    });
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();
    assert_eq!(capture.events.borrow().len(), 1);

    *capture.failure.borrow_mut() = None;
    capture.granted.set(true);
    permission_event(cx);
    assert_eq!(*capture.events.borrow(), ["open a", "open a"]);
    view.read_with(cx, |view, _| {
        assert!(
            matches!(&view.lifecycle, PreviewLifecycle::Streaming { target, .. } if target == "a")
        );
    });
}

#[gpui::test]
fn switching_stopping_and_dropping_release_streams_and_cancel_repaint_pumps(
    cx: &mut TestAppContext,
) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    tick(cx);
    assert_eq!(capture.frames.borrow()[0].polls.get(), 1);

    view.update(cx, |view, cx| view.set_target(Some("b".into()), cx));
    assert_eq!(*capture.events.borrow(), ["open a", "drop a", "open b"]);
    tick(cx);
    // A detached old task would poll the replacement stream too.
    assert_eq!(capture.frames.borrow()[0].polls.get(), 1);
    assert_eq!(capture.frames.borrow()[1].polls.get(), 1);

    view.update(cx, |view, cx| view.set_target(None, cx));
    assert_eq!(
        *capture.events.borrow(),
        ["open a", "drop a", "open b", "drop b"]
    );
    tick(cx);
    view.update(cx, |view, cx| view.set_target(Some("c".into()), cx));
    tick(cx);
    assert_eq!(capture.frames.borrow()[1].polls.get(), 1);
    assert_eq!(capture.frames.borrow()[2].polls.get(), 1);

    let weak = view.downgrade();
    drop(view);
    cx.update(|_| {}); // Flush GPUI's deferred entity release.
    cx.run_until_parked();
    tick(cx);
    assert!(
        weak.upgrade().is_none(),
        "the repaint task must not retain its view"
    );
    assert_eq!(capture.frames.borrow()[2].polls.get(), 1);
    assert_eq!(
        *capture.events.borrow(),
        ["open a", "drop a", "open b", "drop b", "open c", "drop c"]
    );
}

#[gpui::test]
fn repaint_notifies_only_for_an_unrendered_generation(cx: &mut TestAppContext) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    let notifies = Rc::new(Cell::new(0));
    let _observer = cx.update(|cx| {
        cx.observe(&view, {
            let notifies = notifies.clone();
            move |_, _| notifies.set(notifies.get() + 1)
        })
    });
    tick(cx);
    assert_eq!(notifies.get(), 0);

    capture.frames.borrow()[0].publish(vec![1, 2, 3, 255, 4, 5, 6, 255]);
    tick(cx);
    assert_eq!(notifies.get(), 1);
    let cx = cx.add_empty_window();
    draw(&view, cx);
    tick(cx);
    assert_eq!(
        notifies.get(),
        1,
        "rendering consumes the pending generation"
    );
}

#[gpui::test]
fn textures_follow_frame_generation_and_are_released_without_another_render(
    cx: &mut TestAppContext,
) {
    let (view, capture) = preview(cx);
    capture.granted.set(true);
    view.update(cx, |view, cx| view.set_target(Some("a".into()), cx));
    let frames = capture.frames.borrow()[0].clone();
    let cx = cx.add_empty_window();
    draw(&view, cx);
    assert_eq!(frames.takes.get(), 0);

    let first_pixels = vec![10, 20, 30, 255, 40, 50, 60, 128];
    frames.publish(first_pixels.clone());
    draw(&view, cx);
    let first = view.read_with(cx, |view, _| view.current_image.clone().unwrap());
    assert_eq!(first.as_bytes(0).unwrap(), first_pixels);
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&first)));
    draw(&view, cx);
    assert_eq!(
        frames.takes.get(),
        1,
        "unchanged generations reuse the texture"
    );
    view.read_with(cx, |view, _| {
        assert_eq!(view.current_image.as_ref().unwrap().id, first.id);
    });

    frames.publish(vec![1; 8]);
    let latest_pixels = vec![90, 80, 70, 255, 60, 50, 40, 255];
    frames.publish(latest_pixels.clone());
    draw(&view, cx);
    let latest = view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming {
                last_generation: 3,
                ..
            }
        ));
        view.current_image.clone().unwrap()
    });
    assert_eq!(latest.as_bytes(0).unwrap(), latest_pixels);
    assert_ne!(latest.id, first.id);
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&first)));
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&latest)));

    frames.publish(vec![0; 3]); // A malformed frame must not replace the last good texture.
    draw(&view, cx);
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming {
                last_generation: 3,
                ..
            }
        ));
        assert_eq!(view.current_image.as_ref().unwrap().id, latest.id);
    });

    view.update(cx, |view, cx| {
        view.set_target(Some("b".into()), cx);
        assert!(view.current_image.is_none());
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming {
                last_generation: 0,
                ..
            }
        ));
    });
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&latest)));
    capture.frames.borrow()[1].publish(vec![7; 8]);
    draw(&view, cx);
    let replacement = view.read_with(cx, |view, _| view.current_image.clone().unwrap());
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&replacement)));
    view.update(cx, |view, cx| view.set_target(None, cx));
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&replacement)));
    view.read_with(cx, |view, _| assert!(view.current_image.is_none()));
}

// ---- Resolution and frame rate ---------------------------------------------

/// What the fake camera takes to let go of a dropped stream. Not the real
/// backend's figure: the tests pin the behaviour around the wait, not its length.
const RELEASE_DELAY: Duration = Duration::from_millis(700);

const CAMERA_ID: &str = "cam-1";

fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
    StreamMode { width, height, fps }
}

/// The default mode of [`streamcam_modes`]: the one nearest 720p at 30 fps.
fn default_mode() -> StreamMode {
    mode(1280, 720, 30)
}

/// What a StreamCam-like camera offers.
fn streamcam_modes() -> StreamModes {
    StreamModes::new([
        mode(1920, 1080, 60),
        mode(1920, 1080, 30),
        mode(1280, 720, 60),
        mode(1280, 720, 30),
        mode(640, 360, 30),
    ])
}

/// A preview over a state whose one camera, [`CAMERA_ID`], is the selected
/// device, and over a capture that grants access and offers the StreamCam's
/// modes. Also returns the camera's config key, where its mode is saved.
fn camera_preview(cx: &mut TestAppContext) -> (Entity<CameraPreview>, Rc<FakeCapture>, String) {
    cx.update(gpui_component::init);
    let key = cx.update(|cx| {
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let camera = Camera {
            name: "Logitech StreamCam".to_string(),
            unique_id: CAMERA_ID.to_string(),
            serial_number: Some("SN-1".to_string()),
            vendor_id: 0x046d,
            product_id: 0x0893,
            max_resolution: None,
            max_fps: None,
        };
        let state = cx.new(|_| {
            AppState::new(Sources {
                cameras: &[camera],
                ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
            })
        });
        let key = state
            .read(cx)
            .current_record()
            .expect("the camera is selected")
            .config_key
            .clone();
        AppState::set_global(state, cx);
        key
    });
    let capture = Rc::new(FakeCapture::default());
    capture.granted.set(true);
    *capture.modes.borrow_mut() = streamcam_modes();
    let view = cx.new(|cx| CameraPreview::with_capture(Box::new(capture.clone()), cx));
    (view, capture, key)
}

fn select_camera(view: &Entity<CameraPreview>, cx: &mut TestAppContext) {
    view.update(cx, |view, cx| view.set_target(Some(CAMERA_ID.into()), cx));
}

/// Save `mode` for the camera as the rows' pills do, announcing the change.
fn pick(cx: &mut TestAppContext, key: &str, mode: StreamMode) {
    cx.update(|cx| AppState::apply(cx, |state| state.commit_camera_stream_mode(key, mode)));
    cx.run_until_parked();
}

fn advance(cx: &mut TestAppContext, by: Duration) {
    cx.executor().advance_clock(by);
    cx.run_until_parked();
}

fn events(capture: &FakeCapture) -> Vec<String> {
    capture.events.borrow().clone()
}

fn rows_shown(view: &Entity<CameraPreview>, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| view.read(cx).mode_rows(theme::palette(cx), cx).is_some())
}

#[gpui::test]
fn a_stream_opens_at_the_saved_mode(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    pick(cx, &key, mode(1920, 1080, 60));

    select_camera(&view, cx);

    assert_eq!(*capture.opened_modes.borrow(), [Some(mode(1920, 1080, 60))]);
}

#[gpui::test]
fn with_nothing_saved_a_stream_opens_at_the_cameras_default(cx: &mut TestAppContext) {
    let (view, capture, _key) = camera_preview(cx);

    select_camera(&view, cx);

    assert_eq!(*capture.opened_modes.borrow(), [Some(default_mode())]);
}

#[gpui::test]
fn a_saved_mode_the_camera_does_not_offer_opens_at_the_default(cx: &mut TestAppContext) {
    // Saved for a camera since swapped out, or one this driver stopped listing.
    let (view, capture, key) = camera_preview(cx);
    pick(cx, &key, mode(3840, 2160, 30));

    select_camera(&view, cx);

    assert_eq!(*capture.opened_modes.borrow(), [Some(default_mode())]);
}

#[gpui::test]
fn a_target_that_is_not_the_selected_camera_opens_at_the_default_without_rows(
    cx: &mut TestAppContext,
) {
    let (view, capture, key) = camera_preview(cx);
    pick(cx, &key, mode(1920, 1080, 60));

    view.update(cx, |view, cx| {
        view.set_target(Some("another-camera".into()), cx);
    });

    assert_eq!(
        *capture.opened_modes.borrow(),
        [Some(default_mode())],
        "another camera must not borrow the selected one's saved mode"
    );
    assert!(
        !rows_shown(&view, cx),
        "its rows would save under the wrong key"
    );
}

#[gpui::test]
fn choosing_another_mode_reopens_the_stream_after_the_camera_lets_go(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    assert_eq!(events(&capture), ["open cam-1"]);

    pick(cx, &key, mode(1920, 1080, 60));
    assert_eq!(
        events(&capture),
        ["open cam-1", "drop cam-1"],
        "the old stream is released at once, the new one not before the delay"
    );
    view.read_with(cx, |view, _| {
        assert!(matches!(&view.lifecycle, PreviewLifecycle::Restarting { target, .. } if target == CAMERA_ID));
    });

    advance(cx, RELEASE_DELAY.saturating_sub(Duration::from_millis(1)));
    assert_eq!(events(&capture), ["open cam-1", "drop cam-1"]);

    advance(cx, Duration::from_millis(1));
    assert_eq!(events(&capture), ["open cam-1", "drop cam-1", "open cam-1"]);
    assert_eq!(
        *capture.opened_modes.borrow(),
        [Some(default_mode()), Some(mode(1920, 1080, 60))]
    );
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming { mode: Some(running), .. } if *running == mode(1920, 1080, 60)
        ));
    });
}

#[gpui::test]
fn a_pick_during_the_wait_replaces_the_one_before_it(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    pick(cx, &key, mode(1920, 1080, 60));
    advance(cx, RELEASE_DELAY / 2);

    pick(cx, &key, mode(640, 360, 30));
    advance(cx, RELEASE_DELAY);

    assert_eq!(
        events(&capture),
        ["open cam-1", "drop cam-1", "open cam-1"],
        "one reopen, not one per pick"
    );
    assert_eq!(
        capture.opened_modes.borrow().last().copied(),
        Some(Some(mode(640, 360, 30))),
        "the reopen reads the latest choice"
    );
}

#[gpui::test]
fn choosing_the_mode_already_running_does_not_reopen(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);

    // Nothing was saved, so the stream runs at the default; saving that same
    // mode explicitly changes nothing the camera would notice.
    pick(cx, &key, default_mode());
    advance(cx, RELEASE_DELAY * 2);

    assert_eq!(events(&capture), ["open cam-1"]);
}

#[gpui::test]
fn an_unrelated_camera_change_does_not_reopen(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);

    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.commit_camera_control(&key, openlogi_camera::CameraControl::Brightness, 10)
        });
    });
    advance(cx, RELEASE_DELAY * 2);

    assert_eq!(events(&capture), ["open cam-1"]);
}

#[gpui::test]
fn a_backend_that_lists_no_modes_never_reopens_and_shows_no_rows(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    *capture.modes.borrow_mut() = StreamModes::default();
    select_camera(&view, cx);

    pick(cx, &key, mode(1920, 1080, 60));
    advance(cx, RELEASE_DELAY * 2);

    assert_eq!(events(&capture), ["open cam-1"]);
    assert_eq!(*capture.opened_modes.borrow(), [None]);
    assert!(!rows_shown(&view, cx));
}

#[gpui::test]
fn the_rows_show_when_the_camera_lists_modes_and_follow_the_stream(cx: &mut TestAppContext) {
    let (view, _capture, key) = camera_preview(cx);
    assert!(
        !rows_shown(&view, cx),
        "nothing to choose before a camera is open"
    );

    select_camera(&view, cx);
    assert!(rows_shown(&view, cx));

    pick(cx, &key, mode(1920, 1080, 60));
    assert!(
        rows_shown(&view, cx),
        "still offered while the camera reopens"
    );

    view.update(cx, |view, cx| view.set_target(None, cx));
    assert!(
        !rows_shown(&view, cx),
        "another camera's modes must not linger"
    );
}

#[gpui::test]
fn the_rows_stay_when_a_mode_fails_to_open_so_another_can_be_picked(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    pick(cx, &key, mode(1920, 1080, 60));
    *capture.failure.borrow_mut() = Some(CaptureError::Setup("refused".into()));

    select_camera(&view, cx);
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::StartFailed { .. }
        ));
    });
    assert!(rows_shown(&view, cx));

    // The retry timer picks the new choice up.
    *capture.failure.borrow_mut() = None;
    pick(cx, &key, mode(640, 360, 30));
    advance(cx, RETRY_INTERVAL);
    assert_eq!(
        capture.opened_modes.borrow().last().copied(),
        Some(Some(mode(640, 360, 30)))
    );
    view.read_with(cx, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Streaming { .. }
        ));
    });
}

#[gpui::test]
fn leaving_during_a_reopen_cancels_it(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    pick(cx, &key, mode(1920, 1080, 60));

    view.update(cx, |view, cx| view.set_target(None, cx));
    advance(cx, RELEASE_DELAY * 2);

    assert_eq!(events(&capture), ["open cam-1", "drop cam-1"]);
    view.read_with(cx, |view, _| {
        assert!(matches!(view.lifecycle, PreviewLifecycle::Stopped));
    });
}

#[gpui::test]
fn a_reopen_waits_for_revoked_access_instead_of_opening(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    pick(cx, &key, mode(1920, 1080, 60));

    capture.granted.set(false);
    advance(cx, RELEASE_DELAY);

    assert_eq!(events(&capture), ["open cam-1", "drop cam-1"]);
    view.read_with(cx, |view, _| {
        assert!(matches!(&view.lifecycle, PreviewLifecycle::AwaitingAccess(id) if id == CAMERA_ID));
    });
}

#[gpui::test]
fn a_reopen_keeps_the_last_frame_on_screen(cx: &mut TestAppContext) {
    let (view, capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    let window = cx.add_empty_window();
    capture.frames.borrow()[0].publish(vec![1, 2, 3, 255, 4, 5, 6, 255]);
    draw(&view, window);
    view.read_with(window, |view, _| assert!(view.current_image.is_some()));

    cx_pick(window, &key, mode(1920, 1080, 60));

    view.read_with(window, |view, _| {
        assert!(matches!(
            &view.lifecycle,
            PreviewLifecycle::Restarting { .. }
        ));
        assert!(
            view.current_image.is_some(),
            "a frozen frame beats a blank box while the camera reopens"
        );
    });
}

/// [`pick`] for a window context, where the test has no `TestAppContext` left.
fn cx_pick(cx: &mut VisualTestContext, key: &str, mode: StreamMode) {
    cx.update(|_, cx| AppState::apply(cx, |state| state.commit_camera_stream_mode(key, mode)));
    cx.run_until_parked();
}

#[gpui::test]
fn the_rows_render_without_a_stream_frame(cx: &mut TestAppContext) {
    // Smoke: the rows are built from the saved mode and the camera's list, in
    // every lifecycle state that shows them.
    let (view, _capture, key) = camera_preview(cx);
    select_camera(&view, cx);
    let window = cx.add_empty_window();
    draw(&view, window);
    cx_pick(window, &key, mode(1920, 1080, 30));
    draw(&view, window);
}

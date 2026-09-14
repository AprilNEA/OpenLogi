use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, point, size};
use openlogi_camera::CaptureError;
use openlogi_core::config::Config;

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::ConfigPersistence;

#[derive(Default)]
struct FakeCapture {
    granted: Cell<bool>,
    failure: RefCell<Option<CaptureError>>,
    events: Rc<RefCell<Vec<String>>>,
    frames: RefCell<Vec<Rc<Frames>>>,
}

impl Capture for Rc<FakeCapture> {
    fn access_granted(&self) -> bool {
        self.granted.get()
    }

    fn start_stream(&self, target: &str) -> Result<Box<dyn PreviewStream>, CaptureError> {
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
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            AppState::with_runtime(
                Config::ephemeral(),
                &[],
                &[],
                &cache,
                &[],
                ConfigPersistence::MemoryOnly,
                commands,
            )
        });
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
        size(px(PREVIEW_W), px(PREVIEW_H)),
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
        assert!(matches!(&view.lifecycle, PreviewLifecycle::StartFailed(id) if id == "a"));
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

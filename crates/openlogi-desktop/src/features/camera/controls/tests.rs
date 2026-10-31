use std::collections::VecDeque;
use std::sync::Mutex;

use gpui::{Entity, TestAppContext, VisualTestContext, point, size};
use openlogi_camera::{AutoState, Camera, CameraState, ControlError};
use openlogi_core::config::Config;

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::Sources;

const UID: &str = "0x1123000046d0893";
const KEY: &str = "camera:046d:0893";

/// Scripted UVC: each read pops the next queued state (an empty queue reads
/// as an unplugged camera); `reject` fails every batched write.
#[derive(Default)]
struct FakeUvc {
    reads: Mutex<VecDeque<CameraState>>,
    reject: Mutex<bool>,
    calls: Mutex<Vec<String>>,
}

impl FakeUvc {
    fn queue(&self, state: CameraState) {
        self.reads.lock().unwrap().push_back(state);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Uvc for FakeUvc {
    fn read_camera_state(&self, uid: &str) -> Result<CameraState, ControlError> {
        self.calls.lock().unwrap().push(format!("read {uid}"));
        self.reads
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(ControlError::NotFound)
    }

    fn apply_settings(
        &self,
        _uid: &str,
        autos: &[(AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> Result<(), ControlError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("apply {autos:?} {values:?}"));
        if *self.reject.lock().unwrap() {
            return Err(ControlError::Unsupported);
        }
        Ok(())
    }

    fn set_control(
        &self,
        _uid: &str,
        control: CameraControl,
        value: i32,
    ) -> Result<(), ControlError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("set {control:?} {value}"));
        Ok(())
    }
}

fn range(min: i32, max: i32, current: i32) -> ControlRange {
    ControlRange {
        min,
        max,
        default: i32::midpoint(min, max),
        current,
        value_mask: None,
    }
}

/// A Brio-class state: brightness, plus exposure gated by its auto mode.
fn brio(brightness: i32, exposure: i32, exposure_auto: bool) -> CameraState {
    CameraState {
        controls: vec![
            (CameraControl::Brightness, range(0, 255, brightness)),
            (CameraControl::Exposure, range(3, 2047, exposure)),
        ],
        autos: vec![(
            AutoToggle::Exposure,
            AutoState {
                current: exposure_auto,
                default: true,
            },
        )],
    }
}

fn panel(
    cx: &mut TestAppContext,
    uvc: Arc<FakeUvc>,
) -> (Entity<CameraControlsPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(|cx| {
        let camera = Camera {
            name: "Logitech BRIO".to_string(),
            unique_id: UID.to_string(),
            serial_number: None,
            vendor_id: 0x046d,
            product_id: 0x0893,
            max_resolution: None,
            max_fps: None,
        };
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let mut state = AppState::new(Sources {
                cameras: std::slice::from_ref(&camera),
                ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
            });
            let _ = state.select_device(0);
            state
        });
        AppState::set_global(state, cx);
    });
    let view = cx.new(|cx| CameraControlsPanel::with_uvc(uvc, cx));
    (view, cx.add_empty_window())
}

fn draw(view: &Entity<CameraControlsPanel>, cx: &mut VisualTestContext) {
    cx.draw(point(px(0.), px(0.)), size(px(480.), px(800.)), |_, _| {
        view.clone().into_any_element()
    });
}

fn shown(view: &Entity<CameraControlsPanel>, cx: &mut VisualTestContext) -> Vec<(String, i32)> {
    view.read_with(cx, |panel, cx| {
        panel
            .sliders
            .iter()
            .map(|s| (format!("{:?}", s.control), s.value(cx)))
            .chain(
                panel
                    .autos
                    .iter()
                    .map(|a| (format!("{:?}Auto", a.toggle), i32::from(a.on))),
            )
            .collect()
    })
}

fn save(cx: &mut VisualTestContext, f: impl FnOnce(&mut AppState)) {
    cx.update(|_, cx| AppState::update(cx, |state, _| f(state)));
}

#[gpui::test]
fn the_read_runs_off_render_and_seeds_the_rows_from_the_published_state(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(140, 300, false));
    let (view, cx) = panel(cx, uvc.clone());

    draw(&view, cx);
    assert!(uvc.calls().is_empty(), "render must not touch the camera");
    assert!(shown(&view, cx).is_empty());

    cx.run_until_parked();
    assert_eq!(uvc.calls(), [format!("read {UID}")]);
    assert_eq!(
        shown(&view, cx),
        [
            ("Brightness".into(), 140),
            ("Exposure".into(), 300),
            ("ExposureAuto".into(), 0)
        ]
    );

    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(uvc.calls().len(), 1, "a built panel does not re-read");
}

#[gpui::test]
fn saved_settings_are_reapplied_and_an_armed_auto_keeps_its_value(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(140, 300, false));
    let (view, cx) = panel(cx, uvc.clone());
    save(cx, |state| {
        let _ = state.commit_camera_control(KEY, CameraControl::Brightness, 90);
        let _ = state.commit_camera_control(KEY, CameraControl::Exposure, 500);
        let _ = state.commit_camera_auto(KEY, AutoToggle::Exposure, true);
    });

    draw(&view, cx);
    cx.run_until_parked();
    // Saved auto-on takes exposure over, so its saved value is not written.
    assert_eq!(
        uvc.calls(),
        [
            format!("read {UID}"),
            "apply [(Exposure, true)] [(Brightness, 90)]".to_string()
        ]
    );
    assert_eq!(
        shown(&view, cx),
        [
            ("Brightness".into(), 90),
            ("Exposure".into(), 500),
            ("ExposureAuto".into(), 1)
        ]
    );

    // Flipping the auto off re-asserts the slider's value in the same write.
    view.update(cx, |panel, cx| panel.toggle_auto(0, cx));
    assert_eq!(
        uvc.calls().last().map(String::as_str),
        Some("apply [(Exposure, false)] [(Exposure, 500)]")
    );
    assert_eq!(shown(&view, cx)[2], ("ExposureAuto".into(), 0));
}

#[gpui::test]
fn a_rejected_batch_rereads_the_device_and_drops_the_active_profile(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(140, 300, false));
    uvc.queue(brio(120, 300, false));
    *uvc.reject.lock().unwrap() = true;
    let (view, cx) = panel(cx, uvc.clone());
    save(cx, |state| {
        let _ = state.commit_camera_control(KEY, CameraControl::Brightness, 90);
        let _ = state.commit_camera_active_profile(KEY, Some("streaming".into()));
    });

    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(
        uvc.calls(),
        [
            format!("read {UID}"),
            "apply [] [(Brightness, 90)]".to_string(),
            format!("read {UID}")
        ]
    );
    assert_eq!(shown(&view, cx)[0], ("Brightness".into(), 120));
    cx.update(|_, cx| {
        assert_eq!(
            AppState::global(cx).read(cx).camera_active_profile(KEY),
            None
        );
    });
}

#[gpui::test]
fn an_unknown_state_after_a_rejected_batch_builds_nothing_and_retries(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(140, 300, false));
    *uvc.reject.lock().unwrap() = true;
    let (view, cx) = panel(cx, uvc.clone());
    save(cx, |state| {
        let _ = state.commit_camera_control(KEY, CameraControl::Brightness, 90);
    });

    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(uvc.calls().len(), 3, "read, rejected apply, failed re-read");
    assert!(
        shown(&view, cx).is_empty(),
        "never the stale pre-write state"
    );

    // Renders inside the retry window do not hammer the camera.
    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(uvc.calls().len(), 3);

    *uvc.reject.lock().unwrap() = false;
    uvc.queue(brio(140, 300, false));
    cx.executor().advance_clock(RETRY_INTERVAL);
    cx.run_until_parked();
    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(
        uvc.calls()[3..],
        [
            format!("read {UID}"),
            "apply [] [(Brightness, 90)]".to_string()
        ]
    );
    assert_eq!(shown(&view, cx)[0], ("Brightness".into(), 90));
}

#[gpui::test]
fn a_failed_write_rebuilds_from_a_fresh_background_read(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(140, 300, true));
    let (view, cx) = panel(cx, uvc.clone());
    draw(&view, cx);
    cx.run_until_parked();

    *uvc.reject.lock().unwrap() = true;
    uvc.queue(brio(140, 300, false));
    view.update(cx, |panel, cx| panel.toggle_auto(0, cx));
    view.read_with(cx, |panel, _| {
        assert!(panel.key.is_none(), "stale rows are dropped");
    });

    draw(&view, cx);
    cx.run_until_parked();
    assert_eq!(shown(&view, cx)[2], ("ExposureAuto".into(), 0));
}

#[test]
fn a_canceled_load_does_not_write_its_saved_settings() {
    use std::sync::atomic::AtomicBool;

    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(brio(100, 500, false));
    let hw = Hardware::new(uvc.clone());
    let saved = Saved {
        autos: Vec::new(),
        values: vec![(CameraControl::Brightness, 200)],
    };

    let loaded = hardware::load(&hw, UID, &saved, &AtomicBool::new(true));

    assert!(loaded.state.is_none());
    assert_eq!(uvc.calls(), vec![format!("read {UID}")]);
}

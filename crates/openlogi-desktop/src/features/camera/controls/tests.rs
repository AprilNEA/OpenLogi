use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Mutex;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, size};
use openlogi_camera::{AutoState, Camera, CameraState, ControlError};
use openlogi_core::config::Config;

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::Sources;

const UID: &str = "0x1123000046d0893";
const KEY: &str = "camera:046d:0893";

/// Scripted UVC: each read pops the next queued state (an empty queue reads
/// as an unplugged camera); `reject` fails every batched write; a set
/// `unreconnectable` is what every reconnect answers.
#[derive(Default)]
struct FakeUvc {
    reads: Mutex<VecDeque<CameraState>>,
    reject: Mutex<bool>,
    unreconnectable: Mutex<Option<ControlError>>,
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

    fn set_microphone(&self, _uid: &str, on: bool) -> Result<(), ControlError> {
        self.calls.lock().unwrap().push(format!("microphone {on}"));
        Ok(())
    }

    fn reconnect(&self, uid: &str) -> Result<(), ControlError> {
        self.calls.lock().unwrap().push(format!("reconnect {uid}"));
        self.unreconnectable
            .lock()
            .unwrap()
            .clone()
            .map_or(Ok(()), Err)
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
        microphone: None,
    }
}

/// Every camera the panel announces as reconnected, while the returned
/// subscription lives.
fn heard_reconnects(
    view: &Entity<CameraControlsPanel>,
    cx: &mut VisualTestContext,
) -> (Rc<RefCell<Vec<String>>>, Subscription) {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    let subscription = cx.update(|_, cx| {
        cx.subscribe(
            view,
            move |_, CameraReconnected(uid): &CameraReconnected, _| {
                sink.borrow_mut().push(uid.clone());
            },
        )
    });
    (heard, subscription)
}

fn panel(
    cx: &mut TestAppContext,
    uvc: Arc<FakeUvc>,
) -> (Entity<CameraControlsPanel>, &mut VisualTestContext) {
    select_camera(cx);
    let view = cx.new(|cx| CameraControlsPanel::with_uvc(uvc, cx));
    (view, cx.add_empty_window())
}

/// The panel as a window's root, so clicks reach its rows.
fn windowed(
    cx: &mut TestAppContext,
    uvc: Arc<FakeUvc>,
) -> (Entity<CameraControlsPanel>, &mut VisualTestContext) {
    select_camera(cx);
    cx.add_window_view(|_, cx| CameraControlsPanel::with_uvc(uvc, cx))
}

/// Make the test camera the selected device.
fn select_camera(cx: &mut TestAppContext) {
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

/// A Brio 500-class state: brightness, plus the field of view set to its
/// narrowest step.
fn framed(field_of_view: i32) -> CameraState {
    CameraState {
        controls: vec![
            (CameraControl::Brightness, range(0, 255, 140)),
            (
                CameraControl::FieldOfView,
                ControlRange {
                    min: 0,
                    max: 2,
                    default: 0,
                    current: field_of_view,
                    value_mask: None,
                },
            ),
        ],
        autos: Vec::new(),
        microphone: None,
    }
}

/// A look preset changes the picture, not the framing: it keeps the field of
/// view the user picked, while Default puts it back to the camera's default.
#[gpui::test]
fn a_look_preset_keeps_the_field_of_view_and_default_resets_it(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(framed(2));
    let (view, cx) = panel(cx, uvc.clone());
    draw(&view, cx);
    cx.run_until_parked();

    cx.update(|window, cx| {
        view.update(cx, |panel, cx| panel.apply_profile("streaming", window, cx));
    });
    let streaming = uvc.calls().last().cloned().unwrap_or_default();
    assert!(
        streaming.contains("(FieldOfView, 2)"),
        "streaming kept the framing: {streaming}"
    );

    cx.update(|window, cx| {
        view.update(cx, |panel, cx| panel.apply_profile("default", window, cx));
    });
    let default = uvc.calls().last().cloned().unwrap_or_default();
    assert!(
        default.contains("(FieldOfView, 0)"),
        "default reset the framing: {default}"
    );
}

/// A camera with every chip row: anti-flicker and field of view as choices,
/// low light and Show Mode as on/off toggles.
fn chips() -> CameraState {
    let mut state = framed(0);
    state.controls.extend([
        (CameraControl::PowerLineFrequency, range(0, 3, 1)),
        (CameraControl::LowLightCompensation, range(0, 1, 0)),
        (CameraControl::ShowMode, range(0, 1, 0)),
    ]);
    state
}

/// The chip rows share their code, so each click must write its own control:
/// it sends that control and value, and the row shows the chip it picked.
#[gpui::test]
fn a_chip_click_writes_its_own_control(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(chips());
    let (view, cx) = windowed(cx, uvc.clone());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();

    for (selector, write) in [
        ("camera-choice-field_of_view-2", "set FieldOfView 2"),
        (
            "camera-choice-power_line_frequency-2",
            "set PowerLineFrequency 2",
        ),
        (
            "camera-toggle-low_light_compensation",
            "set LowLightCompensation 1",
        ),
        ("camera-toggle-show_mode", "set ShowMode 1"),
    ] {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let chip = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        cx.simulate_click(chip.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(uvc.calls().last().map(String::as_str), Some(write));
    }

    let shown = shown(&view, cx);
    for (control, value) in [
        ("FieldOfView", 2),
        ("PowerLineFrequency", 2),
        ("LowLightCompensation", 1),
        ("ShowMode", 1),
    ] {
        assert!(
            shown.contains(&(control.to_string(), value)),
            "{control} shows {value}: {shown:?}"
        );
    }
}

/// A Brio with its built-in microphone switched off.
fn muted() -> CameraState {
    CameraState {
        microphone: Some(false),
        ..brio(128, 500, true)
    }
}

/// Switching the microphone stores the switch, then reconnects the camera so
/// the switch is in effect; the row shows the new position with no note.
#[gpui::test]
fn a_microphone_switch_stores_it_then_reconnects(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    uvc.queue(muted());
    let (view, cx) = panel(cx, uvc.clone());
    draw(&view, cx);
    cx.run_until_parked();
    view.read_with(cx, |panel, _| assert_eq!(panel.microphone, Some(false)));
    let (heard, _subscription) = heard_reconnects(&view, cx);

    view.update(cx, CameraControlsPanel::toggle_microphone);
    cx.run_until_parked();

    let calls = uvc.calls();
    assert_eq!(
        calls[calls.len() - 2..],
        ["microphone true".to_string(), format!("reconnect {UID}")]
    );
    view.read_with(cx, |panel, _| {
        assert_eq!(panel.microphone, Some(true));
        assert!(matches!(
            panel.microphone_status,
            MicrophoneStatus::Done(Switched::Reconnected)
        ));
    });
    assert_eq!(
        *heard.borrow(),
        [UID.to_string()],
        "the reconnect is announced so the preview can reopen its stream"
    );
}

/// A camera the panel cannot reconnect (no elevation, or the user declined
/// it) still holds the new switch, so the row shows it and asks for a replug.
#[gpui::test]
fn a_camera_that_cannot_reconnect_asks_for_a_replug(cx: &mut TestAppContext) {
    let uvc = Arc::new(FakeUvc::default());
    *uvc.unreconnectable.lock().unwrap() = Some(ControlError::NeedsElevation);
    uvc.queue(muted());
    let (view, cx) = panel(cx, uvc.clone());
    draw(&view, cx);
    cx.run_until_parked();
    let (heard, _subscription) = heard_reconnects(&view, cx);

    view.update(cx, CameraControlsPanel::toggle_microphone);
    cx.run_until_parked();

    view.read_with(cx, |panel, _| {
        assert_eq!(panel.microphone, Some(true));
        assert!(matches!(
            panel.microphone_status,
            MicrophoneStatus::Done(Switched::NeedsReplug)
        ));
    });
    assert!(
        heard.borrow().is_empty(),
        "a camera that stayed on the bus kept its stream"
    );
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

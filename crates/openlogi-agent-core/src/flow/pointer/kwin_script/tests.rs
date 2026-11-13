use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use openlogi_flow::generated as proto;
use openlogi_hook::{DesktopPoint as HookPoint, DisplayBounds, KwinCrossing, KwinSide, WarpError};
use tokio::sync::mpsc;

use super::{EdgeScript, ScriptLoop, place, run};
use crate::flow::edge::{ArmedSides, EdgeSide};
use crate::flow::pointer::{PointerSettings, PointerThread};

/// Two displays as KDE reported them on a laptop beside a wide monitor: the
/// monitor on the right, the laptop panel lower and to its left.
const LAYOUT: [(f64, f64, f64, f64); 2] =
    [(2048.0, 0.0, 3440.0, 1440.0), (0.0, 160.0, 2048.0, 1280.0)];

fn displays() -> Vec<DisplayBounds> {
    LAYOUT
        .iter()
        .map(|&(x, y, width, height)| DisplayBounds {
            x,
            y,
            width,
            height,
        })
        .collect()
}

fn call(side: KwinSide, x: f64, y: f64) -> KwinCrossing {
    KwinCrossing {
        side,
        position: HookPoint { x, y },
        displays: displays(),
    }
}

fn armed(sides: &[EdgeSide]) -> ScriptLoop {
    let mut state = ScriptLoop::default();
    state.configure(PointerSettings {
        sides: ArmedSides::from_sides(sides.iter().copied()),
        require_modifier: false,
    });
    state
}

#[test]
fn a_crossing_maps_onto_the_sides_cumulative_length() {
    let mut state = armed(&[EdgeSide::Left]);

    // The monitor's left edge shows only above the panel (0..160); the
    // panel's own left edge shows over its whole height (160..1440).
    let panel = state
        .crossed(&call(KwinSide::Left, 0.0, 800.0))
        .expect("a crossing on the panel's edge");
    let stub = state
        .crossed(&call(KwinSide::Left, 2048.0, 100.0))
        .expect("a crossing on the monitor's stub");

    assert_eq!(panel.side, EdgeSide::Left);
    assert!((panel.t - 800.0 / 1440.0).abs() < 1e-9, "t was {}", panel.t);
    assert!((stub.t - 100.0 / 1440.0).abs() < 1e-9, "t was {}", stub.t);
}

#[test]
fn the_right_edge_maps_to_the_middle_of_the_monitor() {
    let mut state = armed(&[EdgeSide::Right]);

    let crossing = state
        .crossed(&call(KwinSide::Right, 5487.0, 720.0))
        .expect("a crossing on the right edge");

    assert_eq!(crossing.side, EdgeSide::Right);
    assert!((crossing.t - 0.5).abs() < 1e-9, "t was {}", crossing.t);
}

#[test]
fn an_unarmed_side_gives_no_crossing() {
    let mut state = armed(&[EdgeSide::Left]);

    assert!(
        state
            .crossed(&call(KwinSide::Right, 5487.0, 720.0))
            .is_none()
    );
    assert!(state.crossed(&call(KwinSide::Top, 100.0, 0.0)).is_none());
}

#[test]
fn nothing_is_armed_until_configured() {
    let mut state = ScriptLoop::default();

    assert!(state.crossed(&call(KwinSide::Left, 0.0, 800.0)).is_none());
}

#[test]
fn requiring_control_gives_no_crossing() {
    let mut state = ScriptLoop::default();
    state.configure(PointerSettings {
        sides: ArmedSides::from_sides([EdgeSide::Left]),
        require_modifier: true,
    });

    assert!(state.crossed(&call(KwinSide::Left, 0.0, 800.0)).is_none());
}

#[test]
fn a_layout_without_the_side_gives_no_crossing() {
    let mut state = armed(&[EdgeSide::Left]);
    let mut empty = call(KwinSide::Left, 0.0, 800.0);
    empty.displays.clear();

    assert!(state.crossed(&empty).is_none());
}

#[test]
fn a_position_off_the_edge_uses_the_nearest_segment() {
    let mut state = armed(&[EdgeSide::Left]);

    // Below every segment: the panel's, which ends at the bottom of the
    // layout, is nearest.
    let crossing = state
        .crossed(&call(KwinSide::Left, 0.0, 5000.0))
        .expect("a crossing at the nearest segment");

    assert!((crossing.t - 1.0).abs() < 1e-9, "t was {}", crossing.t);
}

#[test]
fn edge_calls_are_ignored_while_quiet_and_resume_after() {
    let mut state = armed(&[EdgeSide::Left]);

    state.quiet_until(Instant::now() + Duration::from_secs(60));
    assert!(state.crossed(&call(KwinSide::Left, 0.0, 800.0)).is_none());

    // A window that has already passed no longer holds anything back.
    state.quiet_until(Instant::now());
    assert!(state.crossed(&call(KwinSide::Left, 0.0, 800.0)).is_some());
}

#[derive(Default)]
struct FakeState {
    calls: VecDeque<KwinCrossing>,
    displays: Vec<DisplayBounds>,
    warps: Vec<(f64, f64)>,
    /// Make every warp fail, as without a placement device.
    warp_fails: bool,
}

/// A session the test drives, shared with the thread under test.
#[derive(Clone, Default)]
struct FakeScript(Arc<Mutex<FakeState>>);

impl FakeScript {
    fn with_layout() -> Self {
        let script = Self::default();
        script.update(|state| state.displays = displays());
        script
    }

    fn update<T>(&self, change: impl FnOnce(&mut FakeState) -> T) -> T {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn push(&self, crossing: KwinCrossing) {
        self.update(|state| state.calls.push_back(crossing));
    }

    fn warps(&self) -> Vec<(f64, f64)> {
        self.update(|state| state.warps.clone())
    }
}

impl EdgeScript for FakeScript {
    fn poll(&mut self) -> Option<KwinCrossing> {
        self.update(|state| state.calls.pop_front())
    }

    fn displays(&mut self) -> Vec<DisplayBounds> {
        self.update(|state| state.displays.clone())
    }

    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError> {
        self.update(|state| {
            if state.warp_fails {
                return Err(WarpError::OutsideDisplays);
            }
            state.warps.push((to.x, to.y));
            Ok(())
        })
    }
}

fn entry(side: proto::Side, t: f64) -> proto::EntryPoint {
    proto::EntryPoint {
        side: side.into(),
        t,
        ..Default::default()
    }
}

#[test]
fn an_arrival_places_the_cursor_two_pixels_inside_the_matching_edge() {
    let mut script = FakeScript::with_layout();

    // Halfway along the left side: the monitor's 160 px stub and the panel's
    // 1280 px make 1440 in all, so 720 is 560 px into the panel's edge.
    place(&mut script, &entry(proto::Side::Left, 0.5));

    let [(x, y)] = script.warps()[..] else {
        panic!("one placement expected");
    };
    assert_eq!(x, 2.0);
    assert!((y - 720.0).abs() < 1e-9, "y was {y}");
}

#[test]
fn an_arrival_on_the_right_edge_lands_inside_the_monitor() {
    let mut script = FakeScript::with_layout();

    place(&mut script, &entry(proto::Side::Right, 0.5));

    let [(x, y)] = script.warps()[..] else {
        panic!("one placement expected");
    };
    assert_eq!(x, 5486.0);
    assert!((y - 720.0).abs() < 1e-9, "y was {y}");
}

#[test]
fn an_arrival_without_a_layout_or_a_side_places_nothing() {
    let mut no_layout = FakeScript::default();
    place(&mut no_layout, &entry(proto::Side::Left, 0.5));
    assert!(no_layout.warps().is_empty());

    let mut script = FakeScript::with_layout();
    place(&mut script, &entry(proto::Side::Unspecified, 0.5));
    place(&mut script, &entry(proto::Side::Left, f64::NAN));
    assert!(script.warps().is_empty());
}

#[test]
fn a_failed_placement_is_not_retried_or_fatal() {
    let mut script = FakeScript::with_layout();
    script.update(|state| state.warp_fails = true);

    place(&mut script, &entry(proto::Side::Left, 0.5));

    assert!(script.warps().is_empty());
}

#[test]
fn the_thread_places_the_cursor_when_told_to() {
    let script = FakeScript::with_layout();
    let (sender, _receiver) = mpsc::channel(4);
    let source = script.clone();
    let thread = PointerThread::spawn_thread(
        move |commands, moved, crossings| run(source, commands, moved, crossings),
        sender,
    )
    .expect("spawn the pointer thread");

    thread.place(entry(proto::Side::Left, 0.5));

    let warps = (0..200)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(10));
            Some(script.warps()).filter(|warps| !warps.is_empty())
        })
        .expect("the thread placed the cursor");
    assert_eq!(warps.len(), 1);
    assert_eq!(warps[0].0, 2.0);

    thread.stop();
}

#[test]
fn an_edge_call_right_after_a_placement_is_not_reported() {
    let script = FakeScript::with_layout();
    let (sender, mut receiver) = mpsc::channel(4);
    let source = script.clone();
    let thread = PointerThread::spawn_thread(
        move |commands, moved, crossings| run(source, commands, moved, crossings),
        sender,
    )
    .expect("spawn the pointer thread");
    thread.configure(PointerSettings {
        sides: ArmedSides::from_sides([EdgeSide::Left]),
        require_modifier: false,
    });

    // The arrival, then the edge firing again at once: the cursor sits two
    // pixels from the edge it was placed on.
    thread.place(entry(proto::Side::Left, 0.5));
    let placed = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        !script.warps().is_empty()
    });
    assert!(placed, "the thread placed the cursor");
    script.push(call(KwinSide::Left, 2.0, 720.0));
    thread.waker().moved();

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        receiver.try_recv().is_err(),
        "the call inside the cooldown was dropped"
    );

    thread.stop();
}

#[test]
fn the_thread_reports_a_crossing_after_a_wake_and_stops_on_request() {
    let script = FakeScript::default();
    let (sender, mut receiver) = mpsc::channel(4);
    let source = script.clone();
    let thread = PointerThread::spawn_thread(
        move |commands, moved, crossings| run(source, commands, moved, crossings),
        sender,
    )
    .expect("spawn the pointer thread");

    thread.configure(PointerSettings {
        sides: ArmedSides::from_sides([EdgeSide::Left]),
        require_modifier: false,
    });
    // A call for an edge nobody is beyond, then one for the armed edge.
    script.push(call(KwinSide::Top, 100.0, 0.0));
    script.push(call(KwinSide::Left, 0.0, 800.0));
    thread.waker().moved();

    let crossing = (0..200)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(10));
            receiver.try_recv().ok()
        })
        .expect("the thread reported the crossing");
    assert_eq!(crossing.side, EdgeSide::Left);
    assert!(
        receiver.try_recv().is_err(),
        "the unarmed edge stayed quiet"
    );

    thread.stop();
}

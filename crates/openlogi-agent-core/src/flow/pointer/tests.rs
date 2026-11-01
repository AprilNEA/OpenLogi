use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use openlogi_core::config::KeyModifiers;
use openlogi_flow::generated as proto;
use openlogi_hook::{DesktopPoint as HookPoint, DisplayBounds, PointerState, WarpError};
use tokio::sync::mpsc;

use super::{
    GEOMETRY_REFRESH, Pointer, PointerLoop, PointerSettings, PointerThread, entry_position,
    exposed_edges,
};
use crate::flow::edge::{ArmedSides, DesktopPoint, EdgeSide};

#[derive(Default)]
struct FakeState {
    position: (f64, f64),
    /// A position the desktop shows only after the next read, as when the
    /// hook reports motion before the desktop applies it.
    catch_up: Option<(f64, f64)>,
    control: bool,
    displays: Vec<DisplayBounds>,
    warps: Vec<(f64, f64)>,
}

/// A pointer whose state the test sets, shared with the thread under test.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn with_displays(displays: &[(f64, f64, f64, f64)]) -> Self {
        let fake = Self::default();
        fake.set_displays(displays);
        fake
    }

    fn update(&self, change: impl FnOnce(&mut FakeState)) {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn set_displays(&self, displays: &[(f64, f64, f64, f64)]) {
        self.update(|state| {
            state.displays = displays
                .iter()
                .map(|&(x, y, width, height)| DisplayBounds {
                    x,
                    y,
                    width,
                    height,
                })
                .collect();
        });
    }

    fn at(&self, x: f64, y: f64, control: bool) {
        self.update(|state| {
            state.position = (x, y);
            state.control = control;
        });
    }

    fn warps(&self) -> Vec<(f64, f64)> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .warps
            .clone()
    }
}

impl Pointer for Fake {
    fn state(&mut self) -> Option<PointerState> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let position = state.position;
        if let Some(caught_up) = state.catch_up.take() {
            state.position = caught_up;
        }
        Some(PointerState {
            position: HookPoint {
                x: position.0,
                y: position.1,
            },
            modifiers: KeyModifiers {
                control: state.control,
                ..KeyModifiers::default()
            },
        })
    }

    fn displays(&mut self) -> Vec<DisplayBounds> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .displays
            .clone()
    }

    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError> {
        self.update(|state| state.warps.push((to.x, to.y)));
        Ok(())
    }
}

const ONE_DISPLAY: &[(f64, f64, f64, f64)] = &[(0.0, 0.0, 100.0, 100.0)];

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn entry(side: proto::Side, t: f64) -> proto::EntryPoint {
    proto::EntryPoint {
        side: side.into(),
        t,
        ..Default::default()
    }
}

fn right_edge(require_modifier: bool) -> PointerSettings {
    PointerSettings {
        sides: ArmedSides::from_sides([EdgeSide::Right]),
        require_modifier,
    }
}

fn assert_point(point: Option<DesktopPoint>, x: f64, y: f64) {
    let point = point.expect("the entry lands on an edge");
    assert!(
        (point.x - x).abs() < 1e-9 && (point.y - y).abs() < 1e-9,
        "expected ({x}, {y}), got ({}, {})",
        point.x,
        point.y
    );
}

#[test]
fn entry_lands_inside_the_display_at_the_cumulative_position() {
    // B sits right of A and covers part of A's right edge.
    let edges = exposed_edges(&[
        DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
        DisplayBounds {
            x: 100.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        },
    ]);
    // Right side: A 0..20 at x=100, B 20..70 at x=200, A 70..100 at x=100.
    assert_point(
        entry_position(&edges, &entry(proto::Side::Right, 0.1)),
        98.0,
        10.0,
    );
    assert_point(
        entry_position(&edges, &entry(proto::Side::Right, 0.5)),
        198.0,
        50.0,
    );
    assert_point(
        entry_position(&edges, &entry(proto::Side::Right, 1.0)),
        98.0,
        98.0,
    );
    assert_point(
        entry_position(&edges, &entry(proto::Side::Left, 0.5)),
        2.0,
        50.0,
    );
    // Top: A 0..100 at y=0, B 100..200 at y=20.
    assert_point(
        entry_position(&edges, &entry(proto::Side::Top, 0.75)),
        150.0,
        22.0,
    );
    assert_point(
        entry_position(&edges, &entry(proto::Side::Bottom, 0.0)),
        2.0,
        98.0,
    );

    assert!(entry_position(&edges, &entry(proto::Side::Unspecified, 0.5)).is_none());
    assert!(entry_position(&edges, &entry(proto::Side::Left, f64::NAN)).is_none());
    assert!(entry_position(&exposed_edges(&[]), &entry(proto::Side::Left, 0.5)).is_none());
}

#[test]
fn a_required_modifier_gates_crossings_and_releasing_it_forgets_the_approach() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    let mut state = PointerLoop::new(fake.clone(), Duration::ZERO);
    assert!(state.sample(ms(1)).is_none(), "nothing is armed yet");
    state.configure(right_edge(true));

    fake.at(50.0, 50.0, true);
    assert!(state.sample(ms(0)).is_none());
    fake.at(99.5, 50.0, false);
    assert!(state.sample(ms(10)).is_none(), "Control is not held");
    fake.at(99.5, 50.0, true);
    assert!(
        state.sample(ms(20)).is_none(),
        "the approach before the release is forgotten"
    );

    fake.at(50.0, 50.0, true);
    assert!(state.sample(ms(30)).is_none());
    fake.at(99.5, 50.0, true);
    let crossing = state
        .sample(ms(40))
        .expect("a fast approach with Control held");
    assert_eq!(crossing.side, EdgeSide::Right);
    assert!((crossing.t - 0.5).abs() < 1e-9);
}

#[test]
fn refresh_replaces_the_edges_only_when_the_layout_changed() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    let mut state = PointerLoop::new(fake.clone(), Duration::ZERO);
    let original = state.edges.clone();

    fake.set_displays(&[(0.0, 0.0, 200.0, 100.0)]);
    state.refresh(GEOMETRY_REFRESH.saturating_sub(ms(1)));
    assert_eq!(state.edges, original, "the refresh is not due yet");

    state.refresh(GEOMETRY_REFRESH);
    assert_ne!(state.edges, original);
    assert_eq!(state.next_refresh(), GEOMETRY_REFRESH * 2);
    let wider = state.edges.clone();

    fake.set_displays(&[]);
    state.refresh(GEOMETRY_REFRESH * 2);
    assert_eq!(state.edges, wider, "an empty answer keeps the last layout");

    state.configure(right_edge(false));
    fake.at(150.0, 50.0, false);
    assert!(state.sample(GEOMETRY_REFRESH * 2).is_none());
    fake.at(199.5, 50.0, false);
    let crossing = state
        .sample(GEOMETRY_REFRESH * 2 + ms(10))
        .expect("the new right edge is armed");
    assert_eq!(crossing.side, EdgeSide::Right);
}

#[test]
fn place_moves_the_cursor_to_the_entry_point() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    let mut state = PointerLoop::new(fake.clone(), Duration::ZERO);
    state.place(&entry(proto::Side::Left, 0.5));
    state.place(&entry(proto::Side::Unspecified, 0.5));
    assert_eq!(fake.warps(), [(2.0, 50.0)]);
}

/// An arrival at a corner must not touch the neighboring edge: sliding along
/// it would start a dwell and send the devices on to another peer.
#[test]
fn a_corner_arrival_does_not_touch_the_neighboring_edge() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    let mut state = PointerLoop::new(fake.clone(), Duration::ZERO);
    state.configure(PointerSettings {
        sides: ArmedSides::from_sides([EdgeSide::Top, EdgeSide::Bottom]),
        require_modifier: false,
    });

    for (t, start) in [(0.0, ms(0)), (1.0, ms(1_000))] {
        state.place(&entry(proto::Side::Left, t));
        let (x, y) = *fake.warps().last().expect("the cursor was placed");
        fake.at(x, y, false);
        assert!(state.sample(start).is_none());
        // The user moves along the entry edge, parallel to the neighbor.
        fake.at(x + 5.0, y, false);
        assert!(
            state.sample(start + ms(300)).is_none(),
            "an arrival at t = {t} landed on a neighboring edge at ({x}, {y})"
        );
    }
}

#[test]
fn the_thread_reports_a_dwell_crossing_and_stops_on_request() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    fake.at(99.5, 50.0, false);
    let (sender, mut crossings) = mpsc::channel(1);
    let connect = {
        let fake = fake.clone();
        move || Some(fake)
    };
    let thread = PointerThread::spawn_with(connect, sender).expect("spawn the pointer thread");
    thread.configure(right_edge(false));
    let waker = thread.waker();

    let started = Instant::now();
    let crossing = loop {
        waker.moved();
        if let Ok(crossing) = crossings.try_recv() {
            break crossing;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no crossing after a 250 ms dwell"
        );
        std::thread::sleep(ms(20));
    };
    assert_eq!(crossing.side, EdgeSide::Right);

    thread.place(entry(proto::Side::Left, 0.5));
    thread.stop();
    assert_eq!(
        fake.warps(),
        [(2.0, 50.0)],
        "commands sent before stop run first"
    );
}

/// A slow approach that stops on the edge sends no more motion, so the
/// thread itself must look again once the dwell is due.
#[test]
fn a_cursor_resting_on_the_edge_crosses_after_one_wake() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    fake.at(99.5, 50.0, false);
    let (sender, mut crossings) = mpsc::channel(1);
    let connect = {
        let fake = fake.clone();
        move || Some(fake)
    };
    let thread = PointerThread::spawn_with(connect, sender).expect("spawn the pointer thread");
    thread.configure(right_edge(false));
    // Let the thread take the settings before the one motion it sees.
    std::thread::sleep(ms(100));
    thread.waker().moved();

    let started = Instant::now();
    let crossing = loop {
        if let Ok(crossing) = crossings.try_recv() {
            break crossing;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no crossing after a 250 ms dwell without further motion"
        );
        std::thread::sleep(ms(10));
    };
    assert_eq!(crossing.side, EdgeSide::Right);
    thread.stop();
}

/// The hook reports motion before the desktop applies it, so the read that
/// motion wakes can still see the old position. The thread must read again
/// once the motion stops, or a cursor that stops on the edge never crosses.
#[test]
fn the_thread_reads_again_after_motion_stops() {
    let fake = Fake::with_displays(ONE_DISPLAY);
    fake.at(50.0, 50.0, false);
    let (sender, mut crossings) = mpsc::channel(1);
    let connect = {
        let fake = fake.clone();
        move || Some(fake)
    };
    let thread = PointerThread::spawn_with(connect, sender).expect("spawn the pointer thread");
    thread.configure(right_edge(false));
    // Let the thread take the settings before the one motion it sees.
    std::thread::sleep(ms(100));
    fake.update(|state| state.catch_up = Some((99.5, 50.0)));
    thread.waker().moved();

    let started = Instant::now();
    let crossing = loop {
        if let Ok(crossing) = crossings.try_recv() {
            break crossing;
        }
        assert!(
            started.elapsed() < GEOMETRY_REFRESH,
            "no crossing after the last motion landed on the edge"
        );
        std::thread::sleep(ms(10));
    };
    assert_eq!(crossing.side, EdgeSide::Right);
    thread.stop();
}

#[test]
fn a_thread_without_a_pointer_exits_and_ignores_commands() {
    let (sender, _crossings) = mpsc::channel(1);
    let thread =
        PointerThread::spawn_with(|| None::<Fake>, sender).expect("spawn the pointer thread");
    thread.configure(right_edge(false));
    thread.waker().moved();
    thread.stop();
}

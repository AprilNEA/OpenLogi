use std::cell::Cell;

use super::*;

const TRAVEL: f64 = 800.0;
const BUTTON: ButtonId = ButtonId::GestureButton;

thread_local! {
    static CHANGES: Cell<u64> = const { Cell::new(0) };
}

/// A desktop with neighbours both ways whose switches land at once.
const PROMPT: SpaceProbe = SpaceProbe {
    neighbors: || Some((true, true)),
    changes: || {
        CHANGES.set(CHANGES.get() + 1);
        Some(CHANGES.get())
    },
};

/// A desktop whose switches never report landing.
const SILENT: SpaceProbe = SpaceProbe {
    neighbors: || Some((true, true)),
    changes: || Some(0),
};

/// The last desktop: nothing after it.
const LAST: SpaceProbe = SpaceProbe {
    neighbors: || Some((true, false)),
    changes: || Some(0),
};

fn session() -> HidppSessionId {
    HidppSessionId::with_epoch("mouse", 1)
}

fn swipes(probe: SpaceProbe) -> SpaceSwipes {
    SpaceSwipes::new(Some(TRAVEL), probe)
}

/// Open the usual Right → Next / Left → Previous transition.
fn begin_right(swipes: &mut SpaceSwipes, now: Instant) -> Frame {
    swipes
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::NextDesktop,
            Some(&Action::PreviousDesktop),
            now,
        )
        .expect("a desktop swipe opens a transition")
}

fn phases(frames: &[Frame]) -> Vec<SpaceSwipePhase> {
    frames.iter().map(|frame| frame.phase).collect()
}

/// Feed `reports` motion reports of `dx`, one every 8 ms from `start`.
fn sweep(swipes: &mut SpaceSwipes, start: Instant, reports: u32, dx: i32) -> (Vec<Frame>, Instant) {
    let mut frames = Vec::new();
    let mut now = start;
    for _ in 0..reports {
        now += Duration::from_millis(8);
        frames.extend(swipes.motion(&session(), BUTTON, dx, now));
    }
    (frames, now)
}

#[test]
fn only_horizontal_desktop_swipes_open_a_transition() {
    let mut swipes = swipes(PROMPT);
    let now = Instant::now();
    for (direction, action) in [
        (GestureDirection::Up, Action::NextDesktop),
        (GestureDirection::Click, Action::NextDesktop),
        (GestureDirection::Right, Action::MissionControl),
    ] {
        assert_eq!(
            swipes.begin(&session(), BUTTON, direction, &action, None, now),
            None
        );
    }
    assert!(!swipes.is_active(&session(), BUTTON));
    assert!(
        SpaceSwipes::new(None, PROMPT)
            .begin(
                &session(),
                BUTTON,
                GestureDirection::Right,
                &Action::NextDesktop,
                None,
                now,
            )
            .is_none(),
        "live transitions off means the one-shot switch"
    );
}

#[test]
fn the_first_frame_carries_the_commit_travel_toward_the_bound_desktop() {
    let now = Instant::now();
    let frame = begin_right(&mut swipes(PROMPT), now);
    assert_eq!(frame.phase, SpaceSwipePhase::Began);
    assert!((frame.progress - f64::from(GESTURE_SWIPE_THRESHOLD) / TRAVEL).abs() < 1e-9);

    // A swapped binding (Right → Previous) drags toward the previous desktop.
    let mut swapped = swipes(PROMPT);
    let frame = swapped
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::PreviousDesktop,
            Some(&Action::NextDesktop),
            now,
        )
        .expect("opened");
    assert!(frame.progress < 0.0);
    let frames = swapped.motion(&session(), BUTTON, 80, now + Duration::from_millis(8));
    assert!(
        frames[0].progress < frame.progress,
        "moving right keeps heading previous"
    );
}

#[test]
fn a_long_sweep_completes_one_switch_per_desktop_width_and_restarts_from_zero() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 50 (commit) + 25 × 100 = 2550 units.
    let (frames, _) = sweep(&mut swipes, start, 25, 100);
    let ended = frames
        .iter()
        .filter(|frame| frame.phase == SpaceSwipePhase::Ended)
        .count();
    assert_eq!(ended, 3);
    // Every next transition starts from the one report that follows the
    // landing — never from the hand's travel during the switch.
    for pair in frames.windows(2) {
        if pair[0].phase == SpaceSwipePhase::Ended {
            assert_eq!(pair[1].phase, SpaceSwipePhase::Began);
            assert!((pair[1].progress - 100.0 / TRAVEL).abs() < 1e-9);
        }
    }
}

#[test]
fn motion_waits_for_the_switch_to_land_then_restarts() {
    let mut swipes = swipes(SILENT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // Cross one desktop (50 + 8 × 100 > 800 units).
    let (frames, now) = sweep(&mut swipes, start, 8, 100);
    assert_eq!(
        frames.last().map(|frame| frame.phase),
        Some(SpaceSwipePhase::Ended)
    );
    // The Dock never reports landing: motion inside the timeout is dropped.
    let (frames, now) = sweep(&mut swipes, now, 10, 100);
    assert!(frames.is_empty(), "no frames while the switch lands");
    // Past the timeout the next transition starts from this report alone.
    let later = now + SETTLE_TIMEOUT;
    let frames = swipes.motion(&session(), BUTTON, 40, later);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Began]);
    assert!((frames[0].progress - 40.0 / TRAVEL).abs() < 1e-9);
    // A release while settling has nothing in flight.
    let mut settling = swipes_settling(start);
    assert!(settling.end(&session(), BUTTON, start).is_empty());
}

fn swipes_settling(start: Instant) -> SpaceSwipes {
    let mut swipes = swipes(SILENT);
    begin_right(&mut swipes, start);
    let _ = sweep(&mut swipes, start, 8, 100);
    swipes
}

#[test]
fn the_last_desktop_holds_at_the_edge_and_springs_back() {
    let mut swipes = swipes(LAST);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (frames, now) = sweep(&mut swipes, start, 30, 100);
    assert!(
        frames
            .iter()
            .all(|frame| frame.phase == SpaceSwipePhase::Changed),
        "no switch is asked of a desktop that does not exist"
    );
    assert!((frames.last().expect("frames").progress - 1.0).abs() < 1e-9);
    // Pulling back responds at once from the edge.
    let back = swipes.motion(&session(), BUTTON, -80, now + Duration::from_millis(8));
    assert!((back[0].progress - 0.9).abs() < 1e-9);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
}

#[test]
fn a_short_slow_drag_springs_back_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 10, 10);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
    assert!(!swipes.is_active(&session(), BUTTON));
}

#[test]
fn a_drag_past_halfway_finishes_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 40, 10);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Ended]);
    assert!(frames[0].progress >= COMPLETE_AT);
}

#[test]
fn a_short_flick_finishes_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 50 + 5 × 30 = 200 units (0.25 desktop) in 40 ms, released at speed.
    let (_, now) = sweep(&mut swipes, start, 5, 30);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(4));
    assert_eq!(
        phases(&frames),
        vec![SpaceSwipePhase::Changed, SpaceSwipePhase::Ended]
    );
    assert!(frames.iter().all(|frame| frame.progress >= COMPLETE_AT));
}

#[test]
fn dragging_back_swings_to_the_other_neighbour_only_when_it_is_bound() {
    let start = Instant::now();
    let mut both = swipes(PROMPT);
    begin_right(&mut both, start);
    let (frames, _) = sweep(&mut both, start, 10, -20);
    assert!(
        frames.last().expect("frames").progress < 0.0,
        "Left → Previous is bound, so dragging back reaches the previous desktop"
    );

    let mut one_way = swipes(PROMPT);
    one_way
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::NextDesktop,
            Some(&Action::MissionControl),
            start,
        )
        .expect("opened");
    let (frames, _) = sweep(&mut one_way, start, 10, -20);
    assert!(
        frames.last().expect("frames").progress.abs() < 1e-9,
        "an unbound reverse stops at the starting Space"
    );
}

#[test]
fn canceling_the_session_springs_back_its_transitions_only() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let other = HidppSessionId::with_epoch("mouse", 2);
    swipes
        .begin(
            &other,
            BUTTON,
            GestureDirection::Left,
            &Action::PreviousDesktop,
            None,
            start,
        )
        .expect("opened");
    assert_eq!(
        phases(&swipes.cancel_session(&session())),
        vec![SpaceSwipePhase::Cancelled]
    );
    assert!(!swipes.is_active(&session(), BUTTON));
    assert!(swipes.is_active(&other, BUTTON));
}

use std::cell::Cell;

use super::*;

const TRAVEL: f64 = 800.0;
const BUTTON: ButtonId = ButtonId::GestureButton;

thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
}

/// A counter that moves on every read: a desktop that changes at once.
fn tick() -> u64 {
    TICKS.set(TICKS.get() + 1);
    TICKS.get()
}

const fn middle(current: u64) -> SpacePosition {
    SpacePosition {
        current,
        has_previous: true,
        has_next: true,
    }
}

/// A desktop with neighbours both ways whose switches land at once.
const PROMPT: SpaceProbe = SpaceProbe {
    position: || Some(middle(tick())),
    changes: || Some(tick()),
};

/// A desktop whose switches never report landing.
const SILENT: SpaceProbe = SpaceProbe {
    position: || Some(middle(7)),
    changes: || Some(0),
};

/// Another display keeps changing Spaces; this one never does.
const ELSEWHERE: SpaceProbe = SpaceProbe {
    position: || Some(middle(7)),
    changes: || Some(tick()),
};

/// The last desktop: nothing after it.
const LAST: SpaceProbe = SpaceProbe {
    position: || {
        Some(SpacePosition {
            current: 7,
            has_previous: true,
            has_next: false,
        })
    },
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
fn a_transition_opens_at_zero_and_the_commit_travel_moves_it() {
    let now = Instant::now();
    let mut opened = swipes(PROMPT);
    let frame = begin_right(&mut opened, now);
    assert_eq!(frame, Frame::new(0.0, SpaceSwipePhase::Began));
    // The commit's own travel arrives as the hold's first motion.
    let frames = opened.motion(&session(), BUTTON, 60, now);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Changed]);
    assert!((frames[0].progress - 60.0 / TRAVEL).abs() < 1e-9);

    // A swapped binding (Right → Previous) drags toward the previous desktop.
    let mut swapped = swipes(PROMPT);
    swapped
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::PreviousDesktop,
            Some(&Action::NextDesktop),
            now,
        )
        .expect("opened");
    let frames = swapped.motion(&session(), BUTTON, 80, now + Duration::from_millis(8));
    assert!(frames[0].progress < 0.0, "moving right heads previous");
}

#[test]
fn the_commit_travel_is_no_speed_sample() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // The commit's travel lands in the same instant the transition opens:
    // it moves the Space but must not read as an instant flick.
    let _ = swipes.motion(&session(), BUTTON, 60, start);
    let frames = swipes.end(&session(), BUTTON, start + Duration::from_millis(4));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
}

#[test]
fn a_long_sweep_completes_one_switch_per_desktop_width_and_restarts_from_zero() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 25 × 100 = 2500 units.
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
    // Cross one desktop (8 × 100 = 800 units).
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

#[test]
fn a_space_change_on_another_display_does_not_end_the_settle() {
    let mut swipes = swipes(ELSEWHERE);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (frames, now) = sweep(&mut swipes, start, 8, 100);
    assert_eq!(
        frames.last().map(|frame| frame.phase),
        Some(SpaceSwipePhase::Ended)
    );
    // Changes keep being reported, but this display's desktop never moved.
    let (frames, _) = sweep(&mut swipes, now, 10, 100);
    assert!(frames.is_empty(), "still waiting for this display's switch");
}

#[test]
fn canceling_one_hold_springs_back_only_a_transition_in_flight() {
    let start = Instant::now();
    let mut in_flight = swipes(PROMPT);
    begin_right(&mut in_flight, start);
    let _ = in_flight.motion(&session(), BUTTON, 200, start + Duration::from_millis(8));
    let frames = in_flight.cancel(&session(), BUTTON);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
    assert!(!in_flight.is_active(&session(), BUTTON));

    let mut settling = swipes_settling(start);
    assert!(
        settling.cancel(&session(), BUTTON).is_empty(),
        "a completed switch has nothing left to spring back"
    );
    assert!(!settling.is_active(&session(), BUTTON));
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
    let (_, now) = sweep(&mut swipes, start, 45, 10);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Ended]);
    assert!(frames[0].progress >= COMPLETE_AT);
}

#[test]
fn a_short_flick_finishes_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 5 × 30 = 150 units (about 0.19 desktop) in 40 ms, released at speed.
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

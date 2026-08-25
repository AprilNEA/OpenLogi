use super::*;
use openlogi_hid::thumbwheel::WheelResolution;

/// The resolutions traced off an MX Master 4 over Bolt: 20 ratchets per
/// revolution natively, 120 increments per revolution diverted.
const TRACED: WheelResolution = WheelResolution {
    native_res: 20,
    diverted_res: 120,
};

/// A wheel whose increments are already native scroll units — what the
/// scaling tests below vary, and what every other test here assumes.
fn unscaled(sensitivity: ThumbwheelSensitivity) -> ScrollScale {
    ScrollScale {
        native_per_increment: 1.0,
        sensitivity,
    }
}

#[test]
fn multiplier_is_unity_at_default_sensitivity() {
    assert!((ThumbwheelSensitivity::DEFAULT.scroll_multiplier() - 1.0).abs() < f32::EPSILON);
    assert!(ThumbwheelSensitivity::from_rounded(28.0).scroll_multiplier() > 1.9);
    assert!(ThumbwheelSensitivity::MIN.scroll_multiplier() < 0.1);
}

#[test]
fn action_threshold_drops_with_sensitivity_and_floors_at_one() {
    assert_eq!(
        ThumbwheelSensitivity::DEFAULT.action_threshold(),
        i32::from(ThumbwheelSensitivity::DEFAULT)
    );
    assert!(
        ThumbwheelSensitivity::MIN.action_threshold()
            > ThumbwheelSensitivity::DEFAULT.action_threshold(),
        "low sensitivity needs more increments"
    );
    assert_eq!(
        ThumbwheelSensitivity::MAX.action_threshold(),
        1,
        "high sensitivity floors at one"
    );
}

/// Diverting the wheel changes the unit it reports in. An MX Master 4
/// sends 120 increments per revolution where native scrolling produced 20
/// ratchets, so a revolution has to keep scrolling 20 units — not 120 —
/// with the sensitivity slider left alone.
#[test]
fn a_revolution_scrolls_its_native_amount_however_finely_the_wheel_reports() {
    let scale = ScrollScale {
        native_per_increment: TRACED.native_per_increment(),
        sensitivity: ThumbwheelSensitivity::DEFAULT,
    };
    let mut dir = WheelDirection::default();
    let now = Instant::now();
    let mut lines = 0;
    for _ in 0..120 {
        if let WheelOutput::Scroll(n) =
            advance(&mut dir, &Action::HorizontalScrollRight, 1, scale, now)
        {
            lines += n;
        }
    }
    assert_eq!(lines, 20, "one revolution is 20 native scroll units");
}

/// The sensitivity slider stays a multiplier *of that native amount*.
#[test]
fn sensitivity_multiplies_the_native_amount() {
    let scale = ScrollScale {
        native_per_increment: TRACED.native_per_increment(),
        sensitivity: ThumbwheelSensitivity::from_rounded(28.0), // 2x
    };
    let mut dir = WheelDirection::default();
    let now = Instant::now();
    let mut lines = 0;
    for _ in 0..120 {
        if let WheelOutput::Scroll(n) =
            advance(&mut dir, &Action::HorizontalScrollRight, 1, scale, now)
        {
            lines += n;
        }
    }
    assert_eq!(lines, 40, "2x sensitivity doubles the native 20");
}

#[test]
fn an_unreported_resolution_leaves_increments_unscaled() {
    assert!((WheelResolution::UNKNOWN.native_per_increment() - 1.0).abs() < f32::EPSILON);
}

#[test]
fn scroll_accumulates_fractionally_at_sub_unity_sensitivity() {
    let mut dir = WheelDirection::default();
    let now = Instant::now();
    // multiplier 0.5: two increments make one whole line.
    let half = ThumbwheelSensitivity::from_rounded(7.0);
    assert_eq!(
        advance(
            &mut dir,
            &Action::HorizontalScrollRight,
            1,
            unscaled(half),
            now
        ),
        WheelOutput::Idle
    );
    assert_eq!(
        advance(
            &mut dir,
            &Action::HorizontalScrollRight,
            1,
            unscaled(half),
            now
        ),
        WheelOutput::Scroll(1)
    );
}

#[test]
fn scroll_left_emits_negative_lines() {
    let mut dir = WheelDirection::default();
    let now = Instant::now();
    assert_eq!(
        advance(
            &mut dir,
            &Action::HorizontalScrollLeft,
            1,
            unscaled(ThumbwheelSensitivity::DEFAULT),
            now
        ),
        WheelOutput::Scroll(-1)
    );
}

#[test]
fn directions_accumulate_independently() {
    let mut up = WheelDirection::default();
    let mut down = WheelDirection::default();
    let now = Instant::now();
    let half = ThumbwheelSensitivity::from_rounded(7.0);
    assert_eq!(
        advance(
            &mut up,
            &Action::HorizontalScrollRight,
            1,
            unscaled(half),
            now
        ),
        WheelOutput::Idle
    );
    assert_eq!(
        advance(
            &mut down,
            &Action::HorizontalScrollLeft,
            1,
            unscaled(half),
            now
        ),
        WheelOutput::Idle
    );
    assert_eq!(
        advance(
            &mut up,
            &Action::HorizontalScrollRight,
            1,
            unscaled(half),
            now
        ),
        WheelOutput::Scroll(1)
    );
}

#[test]
fn custom_action_fires_on_threshold_then_respects_cooldown() {
    let mut dir = WheelDirection::default();
    let now = Instant::now();
    for _ in 0..i32::from(ThumbwheelSensitivity::DEFAULT) - 1 {
        assert_eq!(
            advance(
                &mut dir,
                &Action::VolumeUp,
                1,
                unscaled(ThumbwheelSensitivity::DEFAULT),
                now
            ),
            WheelOutput::Idle
        );
    }
    assert_eq!(
        advance(
            &mut dir,
            &Action::VolumeUp,
            1,
            unscaled(ThumbwheelSensitivity::DEFAULT),
            now
        ),
        WheelOutput::FireAction
    );
    for _ in 0..i32::from(ThumbwheelSensitivity::DEFAULT) {
        assert_eq!(
            advance(
                &mut dir,
                &Action::VolumeUp,
                1,
                unscaled(ThumbwheelSensitivity::DEFAULT),
                now
            ),
            WheelOutput::Idle
        );
    }
}

#[test]
fn none_action_is_suppressed() {
    let mut dir = WheelDirection::default();
    assert_eq!(
        advance(
            &mut dir,
            &Action::None,
            5,
            unscaled(ThumbwheelSensitivity::DEFAULT),
            Instant::now()
        ),
        WheelOutput::Idle
    );
}

fn session_id(epoch: u64) -> HidppSessionId {
    HidppSessionId::new("mouse-a", epoch)
}

fn stopped_session_with_epoch(epoch: u64) -> RunningSession {
    RunningSession {
        id: session_id(epoch),
        target: SessionTarget {
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc548,
            },
            spec: CaptureSpec::default(),
            rearm_generation: 0,
        },
        stop: None,
    }
}

fn live_session_with_epoch(epoch: u64) -> RunningSession {
    let (stop, _rx) = oneshot::channel();
    RunningSession {
        stop: Some(stop),
        ..stopped_session_with_epoch(epoch)
    }
}

#[test]
fn rearms_when_the_current_session_dies() {
    assert_eq!(
        on_done(&session_id(7), Some(&live_session_with_epoch(7))),
        DoneAction::Remove { unexpected: true }
    );
}

#[test]
fn ignores_a_stale_session_superseded_by_a_restart() {
    assert_eq!(
        on_done(&session_id(6), Some(&live_session_with_epoch(7))),
        DoneAction::Ignore
    );
}

#[test]
fn ignores_a_completion_from_another_device_at_the_same_epoch() {
    assert_eq!(
        on_done(
            &HidppSessionId::new("mouse-b", 7),
            Some(&live_session_with_epoch(7))
        ),
        DoneAction::Ignore
    );
}

#[test]
fn ignores_a_completion_for_an_untracked_device() {
    assert_eq!(on_done(&session_id(7), None), DoneAction::Ignore);
}

#[test]
fn settles_a_draining_session_quietly() {
    assert_eq!(
        on_done(&session_id(7), Some(&stopped_session_with_epoch(7))),
        DoneAction::Remove { unexpected: false }
    );
}

#[test]
fn accepts_inputs_only_from_the_current_live_session() {
    assert!(accepts_input(
        &session_id(7),
        Some(&live_session_with_epoch(7))
    ));
    assert!(
        !accepts_input(&session_id(6), Some(&live_session_with_epoch(7))),
        "a superseded session's queued input is stale"
    );
    assert!(
        !accepts_input(&session_id(7), Some(&stopped_session_with_epoch(7))),
        "a draining session was already canceled"
    );
    assert!(!accepts_input(&session_id(7), None));
}

#[test]
fn rejects_input_after_the_published_capture_plan_changes() {
    let mut session = live_session_with_epoch(7);
    let mut plan = crate::capture_plan::plan_for_device(
        &openlogi_core::config::Config::default(),
        "mouse-a",
        session.target.route.clone(),
        None,
        0,
    );
    session.target.spec = spec_for(&plan);
    assert!(session_matches_plan(&session, &plan));

    plan.rearm_generation = 1;
    assert!(
        !session_matches_plan(&session, &plan),
        "an input queued before a capture-plan epoch change is stale"
    );
}

//! Synthetic motion-model traces. These values are algorithm fixtures, not
//! measurements captured from physical hardware.

use super::*;

fn source() -> ScrollSource {
    ScrollSource::current_hook()
}

fn hidpp_source(device_key: &str, epoch: u64) -> ScrollSource {
    ScrollSource::Hidpp(HidppSessionId::with_epoch(device_key, epoch))
}

fn wheel(x: f64, y: f64) -> WheelDelta {
    WheelDelta { x, y }
}

fn cumulative(frames: &[ScrollFrame]) -> WheelDelta {
    frames
        .iter()
        .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta))
}

fn assert_delta(actual: WheelDelta, expected: WheelDelta) {
    const EPSILON: f64 = 1.0e-12;
    assert!(
        (actual.x - expected.x).abs() < EPSILON,
        "x: {} != {}",
        actual.x,
        expected.x
    );
    assert!(
        (actual.y - expected.y).abs() < EPSILON,
        "y: {} != {}",
        actual.y,
        expected.y
    );
}

#[test]
fn synthetic_ratchet_trace_follows_cubic_smoothstep_and_finishes_exactly() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        frames.push(frame);
    });

    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 0.15625));

    engine.advance_due(base + Duration::from_millis(50), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 0.5));

    engine.advance_due(base + Duration::from_millis(100), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 1.0));
    assert_eq!(
        frames.first().map(|frame| frame.phase),
        Some(SmoothScrollPhase::Began)
    );
    assert_eq!(
        frames.last().map(|frame| frame.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_high_resolution_burst_retargets_without_losing_distance() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for (millis, delta) in [(0, 0.25), (10, 0.25), (20, 0.25)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(120), &mut |frame| {
        frames.push(frame);
    });

    assert_delta(cumulative(&frames), wheel(0.0, 0.75));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_reversal_crosses_the_current_position_and_conserves_net_input() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(
        source(),
        wheel(0.0, -1.5),
        base + Duration::from_millis(40),
        &mut |frame| frames.push(frame),
    );
    assert_delta(cumulative(&frames), wheel(0.0, 0.352));

    engine.advance_due(base + Duration::from_millis(140), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, -0.5));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_sparse_impulses_form_separate_finite_segments() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(100), &mut |frame| {
        frames.push(frame);
    });
    assert!(engine.active.is_empty());

    engine.impulse(
        source(),
        wheel(0.0, 2.0),
        base + Duration::from_millis(300),
        &mut |frame| frames.push(frame),
    );
    engine.advance_due(base + Duration::from_millis(400), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 3.0));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_delayed_frames_use_absolute_time_not_frame_count() {
    let base = Instant::now();
    let mut dense = ScrollEngine::default();
    let mut dense_frames = Vec::new();
    dense.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        dense_frames.push(frame);
    });
    for millis in (8..=80).step_by(8) {
        dense.advance_due(base + Duration::from_millis(millis), &mut |frame| {
            dense_frames.push(frame);
        });
    }

    let mut delayed = ScrollEngine::default();
    let mut delayed_frames = Vec::new();
    delayed.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        delayed_frames.push(frame);
    });
    delayed.advance_due(base + Duration::from_millis(80), &mut |frame| {
        delayed_frames.push(frame);
    });
    assert_delta(cumulative(&dense_frames), cumulative(&delayed_frames));

    dense.advance_due(base + Duration::from_millis(150), &mut |frame| {
        dense_frames.push(frame);
    });
    delayed.advance_due(base + Duration::from_millis(150), &mut |frame| {
        delayed_frames.push(frame);
    });
    assert_delta(cumulative(&dense_frames), wheel(0.0, 1.0));
    assert_delta(cumulative(&delayed_frames), wheel(0.0, 1.0));
}

#[test]
fn synthetic_opposing_impulses_cancel_before_output() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(source(), wheel(0.0, -1.0), base, &mut |frame| {
        frames.push(frame);
    });

    assert!(frames.is_empty());
    assert!(engine.active.is_empty());
}

#[test]
fn only_finite_nonzero_wheel_ticks_enter_the_model() {
    assert_eq!(
        WheelDelta::try_from(ScrollDelta::wheel_ticks(0.25, -1.0)),
        Ok(wheel(0.25, -1.0))
    );
    WheelDelta::try_from(ScrollDelta::pixels(0.0, 1.0)).unwrap_err();
    WheelDelta::try_from(ScrollDelta::wheel_ticks(0.0, 0.0)).unwrap_err();
    WheelDelta::try_from(ScrollDelta::wheel_ticks(f64::NAN, 1.0)).unwrap_err();
}

#[test]
fn cancellation_emits_one_terminal_phase_only_after_output_began() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(1.0, 0.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.cancel_all(&mut |frame| frames.push(frame));
    assert!(frames.is_empty());

    engine.impulse(source(), wheel(1.0, 0.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });
    engine.cancel_all(&mut |frame| frames.push(frame));
    assert_eq!(
        frames.last().map(|frame| frame.phase),
        Some(SmoothScrollPhase::Cancelled)
    );
    assert_delta(cumulative(&frames), wheel(0.15625, 0.0));
}

#[test]
fn concurrent_sources_share_one_balanced_output_stream() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(first, wheel(1.0, 0.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(second, wheel(0.0, 1.0), base, &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + ANIMATION_DURATION, &mut |frame| {
        frames.push(frame);
    });

    assert_delta(cumulative(&frames), wheel(1.0, 1.0));
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.phase == SmoothScrollPhase::Began)
            .count(),
        1
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.phase == SmoothScrollPhase::Ended)
            .count(),
        1
    );
    assert!(
        frames
            .iter()
            .all(|frame| { !matches!(frame.phase, SmoothScrollPhase::Cancelled) })
    );
    assert!(engine.active.is_empty());
}

#[test]
fn source_cancellation_does_not_interrupt_another_source() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(first.clone(), wheel(1.0, 0.0), base, &mut |_| {});
    engine.impulse(second.clone(), wheel(0.0, 1.0), base, &mut |_| {});
    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });

    engine.cancel_source(&first, &mut |frame| frames.push(frame));
    assert!(!engine.active.contains_key(&first));
    assert!(engine.active.contains_key(&second));
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.phase == SmoothScrollPhase::Cancelled)
            .count(),
        0,
        "a source-local cancellation cannot terminate the shared output stream"
    );

    engine.advance_due(base + ANIMATION_DURATION, &mut |frame| frames.push(frame));
    assert!(engine.active.is_empty());
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.phase == SmoothScrollPhase::Ended)
            .count(),
        1,
        "the other device completes normally"
    );
}

fn phases(frames: &[ScrollFrame]) -> Vec<SmoothScrollPhase> {
    frames.iter().map(|frame| frame.phase).collect()
}

#[test]
fn phased_ticks_are_emitted_at_once_and_end_after_the_idle_window() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for (millis, x) in [(0, 1.0), (30, 2.0), (60, -1.0)] {
        let at = base + Duration::from_millis(millis);
        engine.phased_impulse(source(), wheel(x, 0.0), at, &mut |frame| frames.push(frame));
    }
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Changed,
            SmoothScrollPhase::Changed
        ]
    );
    assert_delta(cumulative(&frames), wheel(2.0, 0.0));

    // Each tick pushes the deadline out, so the gesture is still open just
    // before the window after the last tick closes.
    let last = base + Duration::from_millis(60);
    assert_eq!(engine.next_deadline(), Some(last + PHASED_IDLE));
    let almost = (last + PHASED_IDLE)
        .checked_sub(Duration::from_millis(1))
        .expect("instant stays representable");
    engine.advance_due(almost, &mut |frame| frames.push(frame));
    assert_eq!(frames.len(), 3);

    engine.advance_due(last + PHASED_IDLE, &mut |frame| frames.push(frame));
    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert_delta(cumulative(&frames), wheel(2.0, 0.0));
    assert_eq!(engine.next_deadline(), None);
    assert!(engine.phased.is_empty());
}

#[test]
fn queued_phased_ticks_start_a_new_gesture_after_an_idle_gap() {
    let base = Instant::now();
    let source = hidpp_source("mouse-a", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(source.clone(), wheel(1.0, 0.0), base, &mut |f| {
        frames.push(f);
    });

    let later = base + PHASED_IDLE + Duration::from_millis(1);
    engine.phased_impulse(source, wheel(2.0, 0.0), later, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Ended,
            SmoothScrollPhase::Began
        ]
    );
    assert_delta(cumulative(&frames), wheel(3.0, 0.0));

    engine.advance_due(later + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Ended,
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Ended
        ]
    );
    assert_delta(cumulative(&frames), wheel(3.0, 0.0));
}

#[test]
fn queued_phased_tick_keeps_another_active_source_in_the_same_gesture() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(first.clone(), wheel(1.0, 0.0), base, &mut |f| {
        frames.push(f);
    });
    engine.phased_impulse(second, wheel(2.0, 0.0), base + PHASED_IDLE / 2, &mut |f| {
        frames.push(f);
    });

    let later = base + PHASED_IDLE + Duration::from_millis(1);
    engine.phased_impulse(first, wheel(3.0, 0.0), later, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Changed,
            SmoothScrollPhase::Changed
        ]
    );
    assert_delta(cumulative(&frames), wheel(6.0, 0.0));

    engine.advance_due(later + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Changed,
            SmoothScrollPhase::Changed,
            SmoothScrollPhase::Ended
        ]
    );
    assert_delta(cumulative(&frames), wheel(6.0, 0.0));
}

#[test]
fn a_new_phased_gesture_begins_again_after_the_previous_one_ended() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(source(), wheel(1.0, 0.0), base, &mut |f| frames.push(f));
    engine.advance_due(base + PHASED_IDLE, &mut |f| frames.push(f));
    let later = base + PHASED_IDLE * 5;
    engine.phased_impulse(source(), wheel(1.0, 0.0), later, &mut |f| frames.push(f));
    engine.advance_due(later + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Ended,
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Ended
        ]
    );
}

#[test]
fn concurrent_phased_sources_share_one_gesture_until_all_are_idle() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(first, wheel(1.0, 0.0), base, &mut |f| frames.push(f));
    let later = base + Duration::from_millis(100);
    engine.phased_impulse(second, wheel(1.0, 0.0), later, &mut |f| frames.push(f));

    // The first source is idle here, but the second is not.
    engine.advance_due(base + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [SmoothScrollPhase::Began, SmoothScrollPhase::Changed]
    );

    engine.advance_due(later + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert_eq!(
        frames
            .iter()
            .filter(|f| f.phase == SmoothScrollPhase::Ended)
            .count(),
        1
    );
}

#[test]
fn cancelling_a_phased_gesture_emits_one_terminal_phase_and_clears_state() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(first.clone(), wheel(1.0, 0.0), base, &mut |f| {
        frames.push(f);
    });
    engine.phased_impulse(second, wheel(1.0, 0.0), base, &mut |f| frames.push(f));

    engine.cancel_source(&first, &mut |f| frames.push(f));
    assert!(
        frames
            .iter()
            .all(|f| f.phase != SmoothScrollPhase::Cancelled),
        "another source is still inside the gesture"
    );

    engine.cancel_all(&mut |f| frames.push(f));
    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Cancelled)
    );
    assert!(engine.phased.is_empty());
    assert_eq!(engine.next_deadline(), None);
}

#[test]
fn a_smooth_motion_finishing_inside_a_phased_gesture_does_not_end_the_stream() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(
        hidpp_source("mouse-a", 1),
        wheel(1.0, 0.0),
        base,
        &mut |f| frames.push(f),
    );
    engine.impulse(
        hidpp_source("mouse-b", 1),
        wheel(1.0, 0.0),
        base,
        &mut |f| {
            frames.push(f);
        },
    );

    // The smooth motion completes first (100 ms) while the phased gesture is
    // still inside its idle window (120 ms), so the stream must stay open.
    engine.advance_due(base + ANIMATION_DURATION, &mut |f| frames.push(f));
    assert!(
        frames.iter().all(|f| f.phase != SmoothScrollPhase::Ended),
        "the phased source still owns the end of the stream"
    );

    engine.advance_due(base + PHASED_IDLE, &mut |f| frames.push(f));
    assert_eq!(
        frames
            .iter()
            .filter(|f| f.phase == SmoothScrollPhase::Ended)
            .count(),
        1
    );
    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert_delta(cumulative(&frames), wheel(2.0, 0.0));
}

#[test]
fn phased_gesture_frames_reach_the_phased_injector() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for millis in [0, 30] {
        engine.phased_impulse(
            hidpp_source("mouse-a", 1),
            wheel(1.0, 0.0),
            base + Duration::from_millis(millis),
            &mut |f| frames.push(f),
        );
    }
    engine.advance_due(base + Duration::from_millis(30) + PHASED_IDLE, &mut |f| {
        frames.push(f);
    });

    assert_eq!(
        phases(&frames),
        [
            SmoothScrollPhase::Began,
            SmoothScrollPhase::Changed,
            SmoothScrollPhase::Ended
        ]
    );
    assert!(
        frames.iter().all(|f| f.phased),
        "a phased gesture's frames, terminal included, keep their phases"
    );

    frames.clear();
    engine.phased_impulse(
        hidpp_source("mouse-a", 1),
        wheel(1.0, 0.0),
        base,
        &mut |f| {
            frames.push(f);
        },
    );
    engine.cancel_all(&mut |f| frames.push(f));
    assert_eq!(
        phases(&frames),
        [SmoothScrollPhase::Began, SmoothScrollPhase::Cancelled]
    );
    assert!(frames.iter().all(|f| f.phased));
}

#[test]
fn smoothed_wheel_frames_stay_phaseless() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, &mut |f| frames.push(f));
    engine.advance_due(base + ANIMATION_DURATION, &mut |f| frames.push(f));

    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert!(frames.iter().all(|f| !f.phased));
}

#[test]
fn smooth_output_inside_an_open_phased_gesture_stays_phased() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.phased_impulse(
        hidpp_source("mouse-a", 1),
        wheel(1.0, 0.0),
        base,
        &mut |f| frames.push(f),
    );
    engine.impulse(
        hidpp_source("mouse-b", 1),
        wheel(1.0, 0.0),
        base,
        &mut |f| {
            frames.push(f);
        },
    );
    engine.advance_due(base + PHASED_IDLE, &mut |f| frames.push(f));

    assert_eq!(
        frames.last().map(|f| f.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert!(
        frames.iter().all(|f| f.phased),
        "the stream keeps the kind it opened with, so the gesture never changes kind"
    );
}

/// An engine with momentum scrolling on at its default settings.
fn momentum_engine() -> ScrollEngine {
    let mut engine = ScrollEngine::default();
    engine.set_momentum(MomentumScrolling {
        enabled: true,
        ..MomentumScrolling::default()
    });
    engine
}

#[test]
fn wheel_output_preserves_the_configured_distance_per_notch() {
    let defaults = MomentumScrolling::default();
    let baseline = f64::from(defaults.pixels_per_notch);
    for distance in [16_u16, defaults.pixels_per_notch, 52] {
        let mut engine = ScrollEngine::default();
        engine.set_momentum(MomentumScrolling {
            enabled: true,
            pixels_per_notch: distance,
            ..defaults
        });
        let start = Instant::now();
        let mut frames = Vec::new();
        engine.wheel_impulse(
            hidpp_source("mouse", 1),
            wheel(0.0, 1.0),
            1.0,
            start,
            &mut |frame| frames.push(frame),
        );
        engine.advance_due(start + Duration::from_millis(300), &mut |frame| {
            frames.push(frame);
        });
        let output = frames
            .into_iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.wheel_delta()));
        assert_delta(output, wheel(0.0, f64::from(distance) / baseline));
    }
    // A wheel that does not use momentum keeps its existing native units.
    let direct = ScrollFrame::new(wheel(0.5, -2.0), SmoothScrollPhase::Changed, false);
    assert_delta(direct.wheel_delta(), wheel(0.5, -2.0));
}

/// A frame's distance in wheel ticks, whatever unit it was posted in.
fn in_ticks(frame: &ScrollFrame) -> f64 {
    match frame.unit {
        FrameUnit::Ticks => frame.delta.y,
        FrameUnit::Pixels { per_tick } => frame.delta.y / per_tick,
    }
}

/// Drive a diverted main wheel through the engine: one report per 7.5 ms
/// poll at `speed(t)` notches/s for `duration`, advancing due frames every
/// millisecond. Answers the instant after the last report.
fn drive_wheel(
    engine: &mut ScrollEngine,
    source: &ScrollSource,
    start: Instant,
    speed: impl Fn(f64) -> f64,
    duration: Duration,
    frames: &mut Vec<ScrollFrame>,
) -> Instant {
    drive_wheel_toward(engine, source, start, 1.0, speed, duration, frames)
}

/// [`drive_wheel`] with the wheel turning toward `direction` (±1). Ticks
/// arrive the way dispatch hands them over: raw notches and the scaled
/// impulse both signed in the scroll direction, here at 2× sensitivity.
fn drive_wheel_toward(
    engine: &mut ScrollEngine,
    source: &ScrollSource,
    start: Instant,
    direction: f64,
    speed: impl Fn(f64) -> f64,
    duration: Duration,
    frames: &mut Vec<ScrollFrame>,
) -> Instant {
    let poll = Duration::from_micros(7_500);
    let mut carried = 0.0;
    let mut next_poll = start;
    let mut at = start;
    let end = start + duration;
    while at < end {
        if at >= next_poll {
            carried += speed(at.duration_since(start).as_secs_f64()) * poll.as_secs_f64();
            let notches = carried.floor();
            if notches >= 1.0 {
                carried -= notches;
                let notches = direction * notches;
                engine.wheel_impulse(
                    source.clone(),
                    wheel(0.0, 2.0 * notches),
                    notches,
                    at,
                    &mut |f| frames.push(f),
                );
            }
            next_poll += poll;
        }
        engine.advance_due(at, &mut |f| frames.push(f));
        at += Duration::from_millis(1);
    }
    at
}

#[test]
fn ratchet_wheel_scrolling_eases_as_before() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 60.0,
        Duration::from_millis(150),
        &mut frames,
    );
    assert!(!frames.is_empty());
    assert!(frames.iter().all(|f| !f.glide), "ratchet produced momentum");
}

#[test]
fn a_free_spin_scrolls_by_momentum_and_a_finger_stop_ends_it_at_once() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    let stopped = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(600),
        &mut frames,
    );
    let momentum = frames.iter().filter(|f| f.glide).count();
    assert!(momentum > 40, "only {momentum} momentum frames");
    assert!(frames.iter().filter(|f| f.glide).all(|f| f.delta.y > 0.0));

    // The finger stops the wheel: no report after `stopped`. Momentum must
    // end within the detector's latency and never resume, and settling ticks
    // must not scroll.
    let mut after = Vec::new();
    let mut at = stopped;
    while at < stopped + Duration::from_millis(300) {
        engine.advance_due(at, &mut |f| after.push((at, f)));
        at += Duration::from_millis(1);
    }
    let last_momentum = after
        .iter()
        .filter(|(_, f)| f.glide)
        .map(|(at, _)| *at)
        .max()
        .unwrap_or(stopped);
    assert!(
        last_momentum.saturating_duration_since(stopped) <= Duration::from_millis(50),
        "momentum ran {:?} past the stop",
        last_momentum.saturating_duration_since(stopped)
    );
    let mut settling = Vec::new();
    engine.wheel_impulse(
        source.clone(),
        wheel(0.0, 1.0),
        1.0,
        stopped + Duration::from_millis(60),
        &mut |f| settling.push(f),
    );
    engine.advance_due(stopped + Duration::from_millis(200), &mut |f| {
        settling.push(f);
    });
    assert!(settling.is_empty(), "settling tick scrolled: {settling:?}");
}

/// Momentum scrolling off: the smoothed wheel eases in ticks exactly as any
/// other wheel, however fast it spins.
#[test]
fn momentum_scrolling_off_eases_every_tick_in_ticks() {
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    drive_wheel(
        &mut engine,
        &hidpp_source("mouse", 1),
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut frames,
    );
    assert!(!frames.is_empty());
    assert!(
        frames
            .iter()
            .all(|f| !f.glide && f.unit == FrameUnit::Ticks)
    );
    assert!(
        !engine.needs_frames(),
        "no display pacing with momentum off"
    );
}

#[test]
fn momentum_follows_the_wheel_direction() {
    for direction in [1.0, -1.0] {
        let mut engine = momentum_engine();
        let mut frames = Vec::new();
        drive_wheel_toward(
            &mut engine,
            &hidpp_source("mouse", 1),
            Instant::now(),
            direction,
            |_| 200.0,
            Duration::from_millis(500),
            &mut frames,
        );
        let momentum: Vec<f64> = frames
            .iter()
            .filter(|f| f.glide)
            .map(|f| f.delta.y)
            .collect();
        assert!(!momentum.is_empty());
        assert!(
            momentum.iter().all(|y| y * direction > 0.0),
            "direction {direction}: momentum went {momentum:?}"
        );
        // Eased ratchet frames before the onset go the same way.
        assert!(frames.iter().all(|f| f.delta.y * direction >= 0.0));
    }
}

/// The glide follows the wheel for as long as it visibly turns, and ends
/// with it.
#[test]
fn a_glide_rides_the_wheel_until_it_stops() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    // A flick to 220 notches/s, then a slow friction spin-down over 5 s.
    let spin = |t: f64| {
        if t < 0.1 {
            2200.0 * t
        } else {
            (220.0 - 40.0 * (t - 0.1)).max(0.0)
        }
    };
    let mut timeline = Vec::new();
    let mut at = start;
    for _ in 0..60 {
        let mut chunk = Vec::new();
        at = drive_wheel(
            &mut engine,
            &source,
            at,
            |t| spin(t + at.duration_since(start).as_secs_f64()),
            Duration::from_millis(100),
            &mut chunk,
        );
        let pixels: f64 = chunk
            .iter()
            .filter(|f| f.glide)
            .map(|f| f.delta.y.abs())
            .sum();
        timeline.push((at.duration_since(start), pixels));
    }
    // The page keeps moving while the wheel visibly turns…
    let stalled: Vec<_> = timeline
        .iter()
        .filter(|(t, pixels)| {
            (Duration::from_millis(300)..=Duration::from_millis(4000)).contains(t) && *pixels == 0.0
        })
        .collect();
    assert!(
        stalled.is_empty(),
        "stalled under a turning wheel: {stalled:?}"
    );
    let last_glide = timeline
        .iter()
        .filter(|(_, pixels)| *pixels > 0.0)
        .map(|(t, _)| *t)
        .max()
        .expect("a glide");
    assert!(
        last_glide <= Duration::from_millis(5800),
        "…and stops with it: {last_glide:?}"
    );
}

#[test]
fn a_zero_glide_length_ends_the_glide_with_the_wheel() {
    let mut engine = ScrollEngine::default();
    engine.set_momentum(MomentumScrolling {
        enabled: true,
        glide_ms: 0,
        ..MomentumScrolling::default()
    });
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    let stopped = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |t| (200.0 - 120.0 * t).max(0.0),
        Duration::from_millis(1700),
        &mut frames,
    );
    let mut late = 0;
    let mut at = stopped + Duration::from_millis(500);
    while at < stopped + Duration::from_secs(2) {
        engine.advance_due(at, &mut |f| late += usize::from(f.glide));
        at += Duration::from_millis(1);
    }
    assert_eq!(late, 0);
}

/// Display-paced output: one step per display frame, nothing from the timer
/// in between, and the timer back in charge if display frames stop.
#[test]
fn display_frames_pace_momentum_one_step_each() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut warmup = Vec::new();
    let mut at = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut warmup,
    );
    assert!(engine.needs_frames());
    engine.set_display_driven(true, at);
    let frame = Duration::from_micros(16_667);
    let mut between = Vec::new();
    let mut steps = Vec::new();
    for _ in 0..6 {
        // The wheel keeps spinning; the engine's timer stays silent.
        at = drive_wheel(&mut engine, &source, at, |_| 200.0, frame, &mut between);
        engine.on_display_frame(at, &mut |f| steps.push(f));
    }
    assert!(
        between.iter().all(|f| !f.glide),
        "timer emitted momentum while display-paced"
    );
    let momentum: Vec<f64> = steps
        .iter()
        .filter(|f| f.glide)
        .map(|f| f.delta.y)
        .collect();
    assert_eq!(momentum.len(), 6, "one momentum step per display frame");
    // Each step is one frame of a glide riding the steadily spinning
    // wheel: even, never jumping by more than a quarter at once.
    assert!(
        momentum
            .windows(2)
            .all(|w| w[1] <= w[0] * 1.25 && w[1] >= w[0] * 0.75),
        "uneven steps: {momentum:?}"
    );

    // Display frames stop: after the fallback the timer emits again.
    let stalled = at + DISPLAY_FALLBACK + Duration::from_millis(5);
    let mut fallback = Vec::new();
    drive_wheel(
        &mut engine,
        &source,
        at,
        |_| 200.0,
        stalled.saturating_duration_since(at),
        &mut fallback,
    );
    assert!(
        fallback.iter().any(|f| f.glide),
        "no fallback after display frames stopped"
    );
}

/// A wheel flicked `flicks` times: each flick spins it to 220 notches/s and
/// friction slows it toward 120 before the next, `gap` apart.
fn repeated_flicks(t: f64, flicks: u32, gap: f64) -> f64 {
    let index = (t / gap).floor().min(f64::from(flicks - 1));
    let since = t - index * gap;
    (220.0 - 250.0 * since).max(120.0)
}

/// Run `flicks` repeated flicks through an engine with the given chain
/// speed-up, and answer the momentum pixels and whether any frame fell back
/// to eased ratchet output after the first glide began.
fn glide_with_chain(repeat_boost: u16, flicks: u32) -> (f64, bool) {
    let mut engine = ScrollEngine::default();
    engine.set_momentum(MomentumScrolling {
        enabled: true,
        repeat_boost,
        ..MomentumScrolling::default()
    });
    let mut frames = Vec::new();
    drive_wheel(
        &mut engine,
        &hidpp_source("mouse", 1),
        Instant::now(),
        |t| repeated_flicks(t, flicks, 0.4),
        Duration::from_secs_f64(0.4 * f64::from(flicks)),
        &mut frames,
    );
    let first_glide = frames.iter().position(|f| f.glide).unwrap_or(frames.len());
    let fell_back = frames[first_glide..]
        .iter()
        .any(|f| !f.glide && f.delta.y != 0.0);
    let pixels = frames.iter().filter(|f| f.glide).map(|f| f.delta.y).sum();
    (pixels, fell_back)
}

#[test]
fn repeated_flicks_chain_and_speed_the_glide_up() {
    for flicks in [3, 6] {
        let (plain, _) = glide_with_chain(100, flicks);
        let (chained, chained_fell_back) = glide_with_chain(135, flicks);
        assert!(
            !chained_fell_back,
            "{flicks} flicks: a chained re-flick fell back to ratchet output"
        );
        assert!(
            chained > plain * 1.25,
            "{flicks} flicks: chained glide {chained:.0} px vs plain {plain:.0} px"
        );
    }
}

#[test]
fn a_push_after_the_glide_has_faded_is_not_chained() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    // A flick that slows to a crawl, then a slow push the same way.
    let start = Instant::now();
    let at = drive_wheel(
        &mut engine,
        &source,
        start,
        |t| (220.0 - 110.0 * t).max(25.0),
        Duration::from_secs(2),
        &mut frames,
    );
    let mut after = Vec::new();
    drive_wheel(
        &mut engine,
        &source,
        at,
        |_| 60.0,
        Duration::from_millis(300),
        &mut after,
    );
    assert!(
        after.iter().any(|f| !f.glide && f.delta.y != 0.0),
        "a slow push after a faded glide should scroll as ratchet output"
    );
}

#[test]
fn the_chain_boost_is_capped() {
    let (six, _) = glide_with_chain(200, 6);
    let (three, _) = glide_with_chain(200, 3);
    // ×2 per flick would be ×64 after six; the cap holds it at ×3, so six
    // flicks glide roughly twice as far as three, not twenty times.
    assert!(
        six < three * 4.0,
        "boost not capped: {six:.0} vs {three:.0}"
    );
}

/// The recorded series of relaxed repeated flicks (`repeat_flicks_recording.txt`),
/// replayed through a display-paced engine: while the wheel keeps turning, the page must keep
/// scrolling — no glide may fade out or be cut under a turning wheel.
#[test]
fn recorded_repeated_flicks_keep_scrolling_while_the_wheel_turns() {
    let recording = include_str!("repeat_flicks_recording.txt");
    let reports: Vec<(Duration, f64)> = recording
        .lines()
        .filter(|l| !l.starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|item| {
            let (us, notches) = item.split_once(':').expect("µs:notches");
            (
                Duration::from_micros(us.parse().expect("µs")),
                notches.parse().expect("notches"),
            )
        })
        .collect();
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    let end = reports.last().expect("reports").0 + Duration::from_millis(300);
    let bin = Duration::from_millis(50);
    let bins = usize::try_from(end.as_millis() / bin.as_millis() + 1).expect("bins");
    let (mut wheel, mut output) = (vec![0.0_f64; bins], vec![0.0_f64; bins]);
    let index = |t: Duration| usize::try_from(t.as_millis() / bin.as_millis()).expect("bin");
    let mut next = reports.iter().peekable();
    let frame = Duration::from_micros(16_667);
    let mut next_frame = frame;
    let mut t = Duration::ZERO;
    while t <= end {
        let at = start + t;
        while let Some((when, notches)) = next.next_if(|(when, _)| *when <= t) {
            wheel[index(*when)] += notches.abs();
            engine.wheel_impulse(
                source.clone(),
                wheel_delta(*notches),
                *notches,
                start + *when,
                &mut |f| {
                    output[index(t)] += f.delta.y.abs();
                },
            );
        }
        if engine.needs_frames() && !engine.display_driven() {
            engine.set_display_driven(true, at);
        }
        if t >= next_frame {
            engine.on_display_frame(at, &mut |f| output[index(t)] += f.delta.y.abs());
            next_frame += frame;
        }
        engine.advance_due(at, &mut |f| output[index(t)] += f.delta.y.abs());
        if !engine.needs_frames() && engine.display_driven() {
            engine.set_display_driven(false, at);
        }
        t += Duration::from_millis(1);
    }
    // Bins where the wheel turned at 60 notches/s or more, after the first
    // second, must all carry output.
    let stalls: Vec<u128> = (20..bins)
        .filter(|&i| wheel[i] >= 3.0 && wheel[i - 1] >= 3.0 && output[i] == 0.0)
        .map(|i| u128::try_from(i).expect("bin") * bin.as_millis())
        .collect();
    assert!(
        stalls.is_empty(),
        "scrolling stalled under a turning wheel at {stalls:?} ms"
    );
}

/// The recorded series of relaxed repeated flicks (`repeat_flicks_recording.txt`),
/// replayed through a display-paced engine: the glide's speed never jumps
/// between frames — a re-flick's speed-up and a faster wheel ease in.
#[test]
fn recorded_repeated_flicks_glide_without_jumps() {
    let recording = include_str!("repeat_flicks_recording.txt");
    let reports: Vec<(Duration, f64)> = recording
        .lines()
        .filter(|l| !l.starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|item| {
            let (us, notches) = item.split_once(':').expect("µs:notches");
            (
                Duration::from_micros(us.parse().expect("µs")),
                notches.parse().expect("notches"),
            )
        })
        .collect();
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    let end = reports.last().expect("reports").0 + Duration::from_millis(300);
    let mut next = reports.iter().peekable();
    let frame = Duration::from_micros(16_667);
    let mut next_frame = frame;
    let mut t = Duration::ZERO;
    // Each display frame's glide speed, px/s, and when the glide before
    // it ended: a change there marks a new glide.
    let mut frames = Vec::new();
    while t <= end {
        let at = start + t;
        while let Some((when, notches)) = next.next_if(|(when, _)| *when <= t) {
            engine.wheel_impulse(
                source.clone(),
                wheel_delta(*notches),
                *notches,
                start + *when,
                &mut |_| {},
            );
        }
        if engine.needs_frames() && !engine.display_driven() {
            engine.set_display_driven(true, at);
        }
        if t >= next_frame {
            engine.on_display_frame(at, &mut |_| {});
            let state = engine.wheels.get(&source);
            frames.push((
                state.and_then(|s| s.glide.as_ref().map(|g| g.velocity)),
                state.and_then(|s| s.last_glide.map(|(ended, _)| ended)),
            ));
            next_frame += frame;
        }
        engine.advance_due(at, &mut |_| {});
        if !engine.needs_frames() && engine.display_driven() {
            engine.set_display_driven(false, at);
        }
        t += Duration::from_millis(1);
    }
    let glides = frames
        .windows(2)
        .filter(|w| w[1].0.is_some() && (w[0].0.is_none() || w[0].1 != w[1].1))
        .count();
    assert!(glides >= 5, "only {glides} glides in five series of flicks");
    // Within one glide, every rise of its speed — a faster wheel, a
    // re-flick's speed-up — is a ramp over several frames, not a step: no
    // single frame carries most of a run of rising frames.
    let mut jumps = Vec::new();
    // A run of rising frames: where it starts, its first speed, and its
    // biggest single-frame rise.
    let mut run: Option<(usize, f64, f64)> = None;
    for (i, w) in frames.windows(2).enumerate() {
        let rise = match (w[0], w[1]) {
            ((Some(before), ended), (Some(after), same)) if ended == same && after > before => {
                Some((before, after))
            }
            _ => None,
        };
        run = match (rise, run) {
            (Some((before, after)), Some((start, from, biggest))) => {
                Some((start, from, biggest.max(after - before)))
            }
            (Some((before, after)), None) => Some((i + 1, before, after - before)),
            (None, Some((start, from, biggest))) => {
                let total = w[0].0.unwrap_or(from) - from;
                if total > from * 0.3 && biggest > total * 0.7 {
                    jumps.push((start, from, from + total, biggest));
                }
                None
            }
            (None, None) => None,
        };
    }
    assert!(
        jumps.is_empty(),
        "the glide jumped between frames: {jumps:?}"
    );
}

/// The wheel's measured speed wobbles with the quantization of its reports.
/// The glide follows the wheel, but that wobble must never jump the page:
/// frame to frame the glide's speed changes only a little.
#[test]
fn the_glide_smooths_the_wobble_of_the_wheel_speed() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    let wobble = |t: f64| {
        if t < 0.1 {
            2000.0 * t
        } else {
            200.0 + 50.0 * (std::f64::consts::TAU * t / 0.06).sin()
        }
    };
    let mut warmup = Vec::new();
    let mut at = drive_wheel(
        &mut engine,
        &source,
        start,
        wobble,
        Duration::from_millis(500),
        &mut warmup,
    );
    engine.set_display_driven(true, at);
    let frame = Duration::from_micros(16_667);
    let mut speeds = Vec::new();
    for _ in 0..60 {
        let from = at.duration_since(start).as_secs_f64();
        at = drive_wheel(
            &mut engine,
            &source,
            at,
            |t| wobble(t + from),
            frame,
            &mut Vec::new(),
        );
        engine.on_display_frame(at, &mut |_| {});
        speeds.push(
            engine
                .wheels
                .get(&source)
                .and_then(|s| s.glide.as_ref())
                .map_or(0.0, |g| g.velocity),
        );
    }
    assert!(
        speeds.iter().all(|v| *v > 0.0),
        "the glide ended: {speeds:?}"
    );
    let worst = speeds
        .windows(2)
        .map(|w| (w[1] / w[0] - 1.0).abs())
        .fold(0.0, f64::max);
    assert!(
        worst < 0.15,
        "the glide's speed changed by {:.0}% in one frame: {speeds:?}",
        worst * 100.0
    );
}

fn wheel_delta(notches: f64) -> WheelDelta {
    WheelDelta { x: 0.0, y: notches }
}

/// A reversal the spring-back lock held and then confirmed scrolls back
/// smoothly: the page goes forward, turns once and goes back, without
/// lurching back and forth as the held ticks are released.
#[test]
fn a_confirmed_reversal_turns_the_page_around_once() {
    let mut engine = ScrollEngine::default();
    engine.set_momentum(MomentumScrolling {
        enabled: true,
        spring_back_ms: 100,
        ..MomentumScrolling::default()
    });
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    // Ratchet ticks one way, then the other way soon after: the first step
    // back is held by the lock, then released with the second.
    let ticks: Vec<(u64, f64)> = vec![
        (0, 1.0),
        (40, 1.0),
        (80, 1.0),
        (120, 1.0),
        (160, 1.0),
        (200, -1.0),
        (230, -1.0),
        (260, -1.0),
        (290, -1.0),
    ];
    let mut position = 0.0_f64;
    let mut path = Vec::new();
    for ms in 0..1200 {
        let at = start + Duration::from_millis(ms);
        let mut record = |f: ScrollFrame| {
            position += in_ticks(&f);
            path.push(position);
        };
        for (_, notches) in ticks.iter().filter(|(when, _)| *when == ms) {
            engine.wheel_impulse(
                source.clone(),
                wheel_delta(*notches),
                *notches,
                at,
                &mut record,
            );
        }
        engine.advance_due(at, &mut record);
    }
    let peak = path.iter().copied().fold(f64::MIN, f64::max);
    let end = *path.last().expect("output");
    let travelled: f64 = path.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() + path[0].abs();
    let ideal = peak + (peak - end);
    assert!(
        travelled <= ideal + 1e-6,
        "the page lurched: travelled {travelled:.3} notches for an ideal {ideal:.3} \
         (peak {peak:.3}, end {end:.3})"
    );
    assert!(end < peak, "the reversal never scrolled back");
    // No catch-up burst at the release: no frame moves further than the
    // eased output's own steps.
    let biggest = path
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f64::max);
    assert!(biggest < 0.45, "a {biggest:.2}-notch jump in one frame");
}

/// With momentum scrolling on, the smoothed wheel's eased output is posted
/// in pixels at the configured distance per notch; any other wheel keeps
/// easing in ticks.
#[test]
fn only_the_smoothed_wheel_scrolls_in_pixels() {
    let mut engine = momentum_engine();
    let start = Instant::now();
    let mut main_wheel = Vec::new();
    engine.wheel_impulse(
        hidpp_source("mouse", 1),
        wheel(0.0, 1.0),
        1.0,
        start,
        &mut |f| main_wheel.push(f),
    );
    let mut other = Vec::new();
    engine.impulse(
        ScrollSource::current_hook(),
        wheel(0.0, 1.0),
        start,
        &mut |f| other.push(f),
    );
    let mut at = start;
    while at < start + Duration::from_millis(300) {
        engine.advance_due(at, &mut |f| {
            if f.unit == FrameUnit::Ticks {
                other.push(f);
            } else {
                main_wheel.push(f);
            }
        });
        at += Duration::from_millis(1);
    }
    let per_tick = f64::from(MomentumScrolling::default().pixels_per_notch);
    assert!(
        main_wheel
            .iter()
            .all(|f| f.unit == FrameUnit::Pixels { per_tick })
    );
    let main_px: f64 = main_wheel.iter().map(|f| f.delta.y).sum();
    assert!(
        (main_px - per_tick).abs() < 1e-9,
        "one notch moved {main_px} px"
    );
    assert!(other.iter().all(|f| f.unit == FrameUnit::Ticks));
    let other_ticks: f64 = other.iter().map(|f| f.delta.y).sum();
    assert!(
        (other_ticks - 1.0).abs() < 1e-9,
        "one tick moved {other_ticks}"
    );
}

/// Momentum scrolling's notch steps are crisp — linear and done within
/// [`CRISP_STEP_DURATION`] — while other wheels keep the smoothstep ease.
#[test]
fn momentum_scrolling_steps_crisply() {
    let mut engine = momentum_engine();
    let start = Instant::now();
    let mut steps = Vec::new();
    engine.wheel_impulse(
        hidpp_source("mouse", 1),
        wheel(0.0, 1.0),
        1.0,
        start,
        &mut |f| steps.push((start, f)),
    );
    let mut at = start;
    while at <= start + ANIMATION_DURATION {
        engine.advance_due(at, &mut |f| steps.push((at, f)));
        at += Duration::from_millis(1);
    }
    let last = steps
        .iter()
        .filter(|(_, f)| f.delta.y != 0.0)
        .map(|(at, _)| *at)
        .max()
        .expect("the step moved");
    assert!(
        last <= start + CRISP_STEP_DURATION,
        "the step ran {:?}",
        last - start
    );
    let moved: Vec<f64> = steps
        .iter()
        .map(|(_, f)| in_ticks(f))
        .filter(|y| *y != 0.0)
        .collect();
    // Linear: every full frame moves the same distance.
    let full = &moved[..moved.len() - 1];
    assert!(
        full.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9),
        "uneven linear steps: {moved:?}"
    );
}

/// With friction the glide slows on its own and ends while the wheel still
/// spins, without starting another; without it, it keeps riding the wheel.
#[test]
fn friction_ends_the_glide_under_a_spinning_wheel() {
    let glide_after = |friction: u8| {
        let mut engine = ScrollEngine::default();
        engine.set_momentum(MomentumScrolling {
            enabled: true,
            friction,
            ..MomentumScrolling::default()
        });
        let source = hidpp_source("mouse", 1);
        let mut warmup = Vec::new();
        let at = drive_wheel(
            &mut engine,
            &source,
            Instant::now(),
            |_| 200.0,
            Duration::from_secs(2),
            &mut warmup,
        );
        let mut late = Vec::new();
        drive_wheel(
            &mut engine,
            &source,
            at,
            |_| 200.0,
            Duration::from_millis(500),
            &mut late,
        );
        late.iter()
            .filter(|f| f.glide)
            .map(|f| f.delta.y)
            .sum::<f64>()
    };
    assert!(
        glide_after(0) > 500.0,
        "without friction the glide rides on"
    );
    assert!(
        glide_after(100).abs() < f64::EPSILON,
        "full friction still gliding after two seconds"
    );
}

/// A finger reversing a free-spinning wheel without a pause first ends the
/// glide, and the page turns around with the wheel instead of gliding on
/// the old way.
#[test]
fn a_reversal_during_a_glide_ends_it_and_scrolls_back() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut forward = Vec::new();
    let reversed_at = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut forward,
    );
    assert!(forward.iter().any(|f| f.glide), "no glide to reverse");
    let mut back = Vec::new();
    drive_wheel_toward(
        &mut engine,
        &source,
        reversed_at,
        -1.0,
        |_| 200.0,
        Duration::from_millis(200),
        &mut back,
    );
    let net: f64 = back.iter().map(in_ticks).sum();
    assert!(net < 0.0, "the page kept going the old way: net {net:.2}");
    // Only while the spring-back guard holds the first step back (one
    // poll at this speed) may the old glide run on; once the page turns,
    // it never goes forward again.
    let turned = back
        .iter()
        .position(|f| f.delta.y < 0.0)
        .expect("the page never turned around");
    let held = back[..turned].iter().filter(|f| f.glide).count();
    assert!(
        held <= 3,
        "the old glide ran {held} frames past the reversal"
    );
    assert!(
        back[turned..].iter().all(|f| f.delta.y <= 0.0),
        "the page went forward again after turning"
    );
}

/// A display link that stops delivering frames hands output back to the
/// frame timer, whose deadlines move forward: the worker must not be left
/// waiting on an expired deadline it would wake for in a loop.
#[test]
fn a_stalled_display_hands_back_to_future_timer_deadlines() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    let at = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut frames,
    );
    engine.set_display_driven(true, at);
    engine.on_display_frame(at, &mut |f| frames.push(f));
    // The wheel keeps spinning, but no display frame arrives for longer
    // than the fallback allows.
    let stalled = drive_wheel(
        &mut engine,
        &source,
        at,
        |_| 200.0,
        DISPLAY_FALLBACK + Duration::from_millis(20),
        &mut frames,
    );
    engine.advance_due(stalled, &mut |f| frames.push(f));
    assert!(
        engine.needs_frames(),
        "the glide ended under a spinning wheel"
    );
    let deadline = engine.next_deadline().expect("motion still needs frames");
    assert!(
        deadline > stalled,
        "deadline {:?} before now: the worker would spin",
        stalled.saturating_duration_since(deadline)
    );
}

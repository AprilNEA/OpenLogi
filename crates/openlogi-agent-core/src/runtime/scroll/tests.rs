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

#[test]
fn rapid_retargeting_lag_stays_bounded_and_fully_catches_up() {
    // Investigates a follow-up hypothesis (design.md): does retargeting
    // (a new tick arriving before the previous ~100ms animation finishes)
    // repeatedly reset the smoothstep curve's velocity, causing emitted
    // position to fall further and further behind true cumulative input
    // during continuous fast-ish ticking? At the real calibrated magnitude
    // (~0.857/tick) and a 20ms tick cadence (faster than the 100ms
    // animation, so every tick retargets), the lag converges to a small,
    // BOUNDED steady state (~8.24 units, ~190ms worth) rather than growing
    // without limit — a normal, generally-imperceptible characteristic of
    // any eased/smoothed animation, not a runaway bug. Far short of the
    // reported "1-2 seconds" of lag, so this mechanism is likely not that
    // symptom's cause.
    const BOOSTED_TICK: f64 = 0.1 * 15.0 * (8.0 / 14.0);
    const TICK_COUNT: u64 = 100;
    const TICK_INTERVAL_MS: u64 = 20;
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();

    let mut true_total = 0.0;
    let mut lags = Vec::new();
    for i in 0..TICK_COUNT {
        let now = base + Duration::from_millis(i * TICK_INTERVAL_MS);
        engine.impulse(source(), wheel(0.0, BOOSTED_TICK), now, &mut |frame| {
            frames.push(frame);
        });
        true_total += BOOSTED_TICK;
        lags.push(true_total - cumulative(&frames).y);
    }

    // The lag over the back half of the run must be bounded (converged),
    // not still growing - if retargeting caused an unbounded stall, the
    // last lag would be much larger than the lag at the run's midpoint.
    let midpoint_lag = lags[lags.len() / 2];
    let final_lag = *lags.last().expect("at least one tick");
    assert!(
        (final_lag - midpoint_lag).abs() < 0.1,
        "lag should have converged by the run's second half, got midpoint \
         {midpoint_lag:.4} vs final {final_lag:.4}"
    );
    assert!(
        final_lag < 15.0,
        "converged lag {final_lag:.4} should stay a small fraction of the \
         {TICK_COUNT} ticks worth of true input ({true_total:.4}), not balloon"
    );

    // Once ticking stops, the animation must fully catch up to the true
    // cumulative input - no distance permanently lost to the lag.
    engine.advance_due(
        base + Duration::from_millis((TICK_COUNT + 1) * TICK_INTERVAL_MS + 200),
        &mut |frame| frames.push(frame),
    );
    assert_delta(cumulative(&frames), wheel(0.0, true_total));
    assert!(engine.active.is_empty());
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

//! Synthetic motion-model traces. These values are algorithm fixtures, not
//! measurements captured from physical hardware.

use super::*;

/// `pulse_curve` fixtures at quarter progress (independently computed).
const PULSE_AT_QUARTER: f64 = 0.379_833_339_323_793;
const PULSE_AT_HALF: f64 = 0.792_393_601_411_713;
const PULSE_AT_EIGHTH: f64 = 0.109_992_273_800_805;

fn source() -> ScrollSource {
    ScrollSource::current_hook()
}

fn hidpp_source(device_key: &str, epoch: u64) -> ScrollSource {
    ScrollSource::Hidpp(HidppSessionId::with_epoch(device_key, epoch))
}

fn wheel(x: f64, y: f64) -> WheelDelta {
    WheelDelta { x, y }
}

fn tuning(step: f64, duration_ms: u64, max_gain: f64) -> MotionTuning {
    MotionTuning {
        step,
        duration: Duration::from_millis(duration_ms),
        max_gain,
        distance_scale: WheelDelta::UNIT,
        preaccelerated: false,
    }
}

/// Step 1×, pre-accelerated input: gain stays off (as the worker forces on
/// that path) and quick reversals cool down.
fn preaccelerated() -> MotionTuning {
    MotionTuning {
        preaccelerated: true,
        ..tuning(1.0, 100, 1.0)
    }
}

/// Step 1×, no acceleration: every tick animates exactly its own distance.
fn neutral() -> MotionTuning {
    tuning(1.0, 100, 1.0)
}

fn cumulative(frames: &[ScrollFrame]) -> WheelDelta {
    frames
        .iter()
        .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta))
}

fn assert_delta(actual: WheelDelta, expected: WheelDelta) {
    const EPSILON: f64 = 1.0e-9;
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
fn pulse_curve_is_normalized_clamped_and_monotone() {
    assert!(pulse_curve(-1.0).abs() < f64::EPSILON);
    assert!(pulse_curve(0.0).abs() < f64::EPSILON);
    assert!((pulse_curve(1.0) - 1.0).abs() < f64::EPSILON);
    assert!((pulse_curve(2.0) - 1.0).abs() < f64::EPSILON);
    assert!((pulse_curve(0.125) - PULSE_AT_EIGHTH).abs() < 1.0e-9);
    assert!((pulse_curve(0.25) - PULSE_AT_QUARTER).abs() < 1.0e-9);
    assert!((pulse_curve(0.5) - PULSE_AT_HALF).abs() < 1.0e-9);

    let mut previous = 0.0;
    for sample in 1..=100 {
        let value = pulse_curve(f64::from(sample) / 100.0);
        assert!(value > previous, "curve dips at sample {sample}");
        previous = value;
    }
}

#[test]
fn accel_gain_follows_the_published_curve() {
    // A notched wheel at one tick per `dt` ms fills the window with `70/dt`
    // ticks, reproducing the classic per-interval fixtures.
    assert!(
        (accel_gain(7.0, 7.0) - 2.0).abs() < f64::EPSILON,
        "dt 10 ms"
    );
    assert!(
        (accel_gain(14.0, 7.0) - 3.5).abs() < f64::EPSILON,
        "dt 5 ms"
    );
    assert!(
        (accel_gain(3.5, 7.0) - 1.25).abs() < f64::EPSILON,
        "dt 20 ms"
    );
    assert!(
        (accel_gain(70.0 / 30.0, 7.0) - 1.0).abs() < f64::EPSILON,
        "dt 30 ms is the neutral rate"
    );
    assert!(
        (accel_gain(1.0, 7.0) - 1.0).abs() < f64::EPSILON,
        "a lone tick never gains"
    );
    assert!(
        (accel_gain(0.0, 7.0) - 1.0).abs() < f64::EPSILON,
        "an empty window never gains"
    );
    assert!(
        (accel_gain(70.0, 7.0) - 7.0).abs() < f64::EPSILON,
        "cap binds"
    );
    assert!(
        (accel_gain(70.0, 1.0) - 1.0).abs() < f64::EPSILON,
        "max_gain 1 disables acceleration"
    );
}

#[test]
fn synthetic_ratchet_tick_travels_step_distance_and_finishes_exactly() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(
        source(),
        wheel(0.0, 1.0),
        base,
        tuning(3.0, 100, 1.0),
        &mut |frame| {
            frames.push(frame);
        },
    );

    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 3.0 * PULSE_AT_QUARTER));

    engine.advance_due(base + Duration::from_millis(50), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 3.0 * PULSE_AT_HALF));

    engine.advance_due(base + Duration::from_millis(100), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 3.0));
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
fn synthetic_burst_superposes_and_conserves_scaled_input() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for (millis, delta) in [(0, 0.25), (10, 0.25), (20, 0.25)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            tuning(2.0, 100, 1.0),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(200), &mut |frame| {
        frames.push(frame);
    });

    assert_delta(cumulative(&frames), wheel(0.0, 1.5));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_fast_ticks_gain_amplitude_deterministically() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // A notched wheel at 10 ms per tick: the k-th tick sees k ticks in the
    // window, gaining `((1 + 30k/70) / 2).max(1)` — the rate ramps as the
    // window fills and reaches the classic curve's 2× at steady state.
    for millis in (0..=70).step_by(10) {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_millis(millis),
            tuning(1.0, 100, 7.0),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });

    // Gains: 1, 1, 16/14, 19/14, 22/14, 25/14, 28/14, 31/14 → 169/14 total.
    assert_delta(cumulative(&frames), wheel(0.0, 169.0 / 14.0));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_frame_interval_ticks_coalesce_and_cap_at_max_gain() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // 40 ticks over two frame intervals: a monster free-spin burst. Ticks
    // inside one frame interval coalesce into the newest pulse, and once the
    // window holds `70 × (2×7 − 1) / 30 ≈ 30.3` ticks the configured cap
    // pins every later gain at 7×.
    for tick in 0..40 {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_micros(tick * 400),
            tuning(1.0, 100, 7.0),
            &mut |frame| frames.push(frame),
        );
    }
    assert_eq!(
        engine
            .active
            .values()
            .map(|motion| motion.pulses.len())
            .sum::<usize>(),
        2,
        "the 16 ms burst merges into one pulse per frame interval"
    );
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });

    // Gains: 1× for ticks 1–2, `(7 + 3k)/14` ramp for 3–30 (sum 113), the
    // 7× cap for 31–40 → 2 + 113 + 70 = 185 total.
    assert_delta(cumulative(&frames), wheel(0.0, 185.0));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_reversal_superposes_and_conserves_net_input() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(
        source(),
        wheel(0.0, -1.5),
        base + Duration::from_millis(40),
        neutral(),
        &mut |frame| frames.push(frame),
    );

    engine.advance_due(base + Duration::from_millis(200), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, -0.5));
    assert_eq!(
        frames.last().map(|frame| frame.phase),
        Some(SmoothScrollPhase::Ended)
    );
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_opposing_impulses_cancel_before_output() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(source(), wheel(0.0, -1.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });

    assert!(frames.is_empty());
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_delayed_frames_use_absolute_time_not_frame_count() {
    let base = Instant::now();
    let mut dense = ScrollEngine::default();
    let mut dense_frames = Vec::new();
    dense.impulse(
        source(),
        wheel(0.0, 1.0),
        base,
        tuning(2.0, 100, 1.0),
        &mut |frame| {
            dense_frames.push(frame);
        },
    );
    for millis in (8..=80).step_by(8) {
        dense.advance_due(base + Duration::from_millis(millis), &mut |frame| {
            dense_frames.push(frame);
        });
    }

    let mut delayed = ScrollEngine::default();
    let mut delayed_frames = Vec::new();
    delayed.impulse(
        source(),
        wheel(0.0, 1.0),
        base,
        tuning(2.0, 100, 1.0),
        &mut |frame| {
            delayed_frames.push(frame);
        },
    );
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
    assert_delta(cumulative(&dense_frames), wheel(0.0, 2.0));
    assert_delta(cumulative(&delayed_frames), wheel(0.0, 2.0));
}

#[test]
fn synthetic_sparse_impulses_form_separate_finite_pulses() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, neutral(), &mut |frame| {
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
        neutral(),
        &mut |frame| frames.push(frame),
    );
    engine.advance_due(base + Duration::from_millis(400), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 3.0));
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
    engine.impulse(source(), wheel(1.0, 0.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    engine.cancel_all(&mut |frame| frames.push(frame));
    assert!(frames.is_empty());

    engine.impulse(source(), wheel(1.0, 0.0), base, neutral(), &mut |frame| {
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
    assert_delta(cumulative(&frames), wheel(PULSE_AT_QUARTER, 0.0));
}

#[test]
fn concurrent_sources_share_one_balanced_output_stream() {
    let base = Instant::now();
    let first = hidpp_source("mouse-a", 1);
    let second = hidpp_source("mouse-b", 1);
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(first, wheel(1.0, 0.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    engine.impulse(second, wheel(0.0, 1.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(25), &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(100), &mut |frame| {
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
    engine.impulse(first.clone(), wheel(1.0, 0.0), base, neutral(), &mut |_| {});
    engine.impulse(
        second.clone(),
        wheel(0.0, 1.0),
        base,
        neutral(),
        &mut |_| {},
    );
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

    engine.advance_due(base + Duration::from_millis(100), &mut |frame| {
        frames.push(frame);
    });
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
    let glide = Duration::from_millis(100);
    engine.impulse(
        hidpp_source("mouse-b", 1),
        wheel(1.0, 0.0),
        base,
        tuning(1.0, 100, 1.0),
        &mut |f| {
            frames.push(f);
        },
    );

    // The smooth motion completes first (100 ms pulse) while the phased gesture
    // is still inside its idle window (120 ms), so the stream must stay open.
    engine.advance_due(base + glide, &mut |f| frames.push(f));
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
    engine.impulse(
        source(),
        wheel(0.0, 1.0),
        base,
        tuning(1.0, 100, 1.0),
        &mut |f| frames.push(f),
    );
    engine.advance_due(base + Duration::from_millis(100), &mut |f| {
        frames.push(f);
    });

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
        tuning(1.0, 100, 1.0),
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

#[test]
fn same_sign_input_never_emits_an_opposing_frame() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for millis in [0, 10, 20, 50] {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_millis(millis),
            tuning(3.0, 360, 7.0),
            &mut |frame| frames.push(frame),
        );
    }
    for millis in (8..=600).step_by(8) {
        engine.advance_due(base + Duration::from_millis(millis), &mut |frame| {
            frames.push(frame);
        });
    }

    assert!(frames.iter().all(|frame| frame.delta.y >= 0.0));
    // Window gains: 1, 1, 16/14, 19/14 → 4.5 ticks at step 3.
    assert_delta(cumulative(&frames), wheel(0.0, 3.0 * 4.5));
    assert!(engine.active.is_empty());
}

#[test]
fn free_spin_burst_keeps_the_pulse_count_bounded() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    let long_glide = tuning(1.0, 1000, 1.0);
    for millis in 0..600 {
        engine.impulse(
            source(),
            wheel(0.0, 0.1),
            base + Duration::from_millis(millis),
            long_glide,
            &mut |frame| frames.push(frame),
        );
    }
    let pulses = engine
        .active
        .values()
        .map(|motion| motion.pulses.len())
        .max()
        .unwrap_or(0);
    assert!(pulses <= MAX_PULSES, "{pulses} pulses exceed the bound");

    engine.advance_due(base + Duration::from_millis(2000), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 60.0));
    assert!(engine.active.is_empty());
}

#[test]
fn a_tick_merged_past_the_pulse_cap_keeps_its_own_duration() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    let long_glide = tuning(1.0, 1000, 1.0);
    // Fill the source to the cap with a steady stream, let it glide for a
    // while, then land one more tick. It merges into the newest pulse, which
    // by now is well into its animation.
    for millis in (0..=512).step_by(8) {
        engine.impulse(
            source(),
            wheel(0.0, 0.1),
            base + Duration::from_millis(millis),
            long_glide,
            &mut |frame| frames.push(frame),
        );
    }
    let motion = engine.active.get(&source()).expect("source is live");
    assert_eq!(motion.pulses.len(), MAX_PULSES);
    let before = cumulative(&frames);
    engine.impulse(
        source(),
        wheel(0.0, 10.0),
        base + Duration::from_millis(900),
        long_glide,
        &mut |frame| frames.push(frame),
    );
    let motion = engine.active.get(&source()).expect("source is live");
    // The merged tick starts from rest at its own arrival and animates for
    // its own full duration instead of finishing at the older pulse's
    // deadline (1512 ms).
    assert_eq!(motion.pulses.len(), MAX_PULSES);
    assert_eq!(motion.ends_at(), Some(base + Duration::from_millis(1900)));
    // Nothing of the new tick's 10 lines was emitted at its own timestamp —
    // only the glide the older pulses had accumulated since 512 ms.
    let at_arrival = cumulative(&frames).minus(before);
    assert!(at_arrival.y < 10.0 * 0.5, "{} jumped ahead", at_arrival.y);

    engine.advance_due(base + Duration::from_millis(2000), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 65.0 * 0.1 + 10.0));
    assert!(engine.active.is_empty());
}

#[test]
fn a_live_duration_change_gives_the_next_tick_its_own_pulse() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, 1.0), base, neutral(), &mut |frame| {
        frames.push(frame);
    });
    // Reloaded to a 500 ms glide 4 ms later: inside the frame window, yet the
    // tick must not merge into (and re-time) motion accepted under the old
    // duration. It animates on its own for the new duration; the first
    // pulse still ends at its original 100 ms.
    engine.impulse(
        source(),
        wheel(0.0, 1.0),
        base + Duration::from_millis(4),
        tuning(1.0, 500, 1.0),
        &mut |frame| frames.push(frame),
    );
    let motion = engine.active.get(&source()).expect("source is live");
    assert_eq!(motion.pulses.len(), 2);
    assert_eq!(
        motion.pulses.iter().map(Pulse::ends_at).collect::<Vec<_>>(),
        [
            base + Duration::from_millis(100),
            base + Duration::from_millis(504)
        ]
    );

    engine.advance_due(base + Duration::from_millis(600), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 2.0));
    assert!(engine.active.is_empty());
}

#[test]
fn acceleration_windows_are_kept_per_axis() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // The same vertical ramp as `synthetic_fast_ticks_gain_amplitude_deterministically`
    // (169/14 total), interleaved with one slow horizontal tick: its own axis
    // window holds a single tick, so it gains nothing from the fast vertical
    // stream sharing the source.
    for millis in (0..=70).step_by(10) {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_millis(millis),
            tuning(1.0, 100, 7.0),
            &mut |frame| frames.push(frame),
        );
    }
    engine.impulse(
        source(),
        wheel(1.0, 0.0),
        base + Duration::from_millis(72),
        tuning(1.0, 100, 7.0),
        &mut |frame| frames.push(frame),
    );
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(1.0, 169.0 / 14.0));
    assert!(engine.active.is_empty());
}

#[test]
fn reversal_cooldown_scales_by_time_since_the_departed_direction() {
    const EPSILON: f64 = 1.0e-9;
    let base = Instant::now();
    let at = |millis| base + Duration::from_millis(millis);
    let mut cooldown = ReversalCooldown::default();
    // First input commits a direction unscaled.
    assert!((cooldown.attenuate(-5.0, at(0)) + 5.0).abs() < EPSILON);
    // A flip 68 ms later (the measured hot case) passes 68/400 of its value.
    assert!((cooldown.attenuate(4.0, at(68)) - 4.0 * 0.17).abs() < EPSILON);
    // The committed new direction keeps ramping against the same reference
    // (2.0 stays below the knee, so only the ramp scales it).
    assert!((cooldown.attenuate(4.0, at(200)) - 4.0 * 0.5).abs() < EPSILON);
    // Once the ramp elapses only the knee compression remains, fading with
    // the corrective burst's age (332 ms of the 2000 ms recovery).
    let compressed = 3.25 + (4.0 - 3.25) * (332.0 / 2000.0);
    assert!((cooldown.attenuate(4.0, at(400)) - compressed).abs() < EPSILON);
    // A tick after the full recovery window runs unscaled.
    assert!((cooldown.attenuate(4.0, at(2068)) - 4.0).abs() < EPSILON);
}

#[test]
fn reversal_compression_tames_the_corrective_burst_and_fades() {
    const EPSILON: f64 = 1.0e-9;
    let base = Instant::now();
    let at = |millis| base + Duration::from_millis(millis);
    let mut cooldown = ReversalCooldown::default();
    assert!((cooldown.attenuate(-5.0, at(0)) + 5.0).abs() < EPSILON);
    // A cold flip (past the ramp) is still compressed above the knee:
    // 8 lines → 3 + 5/4 at the burst's start.
    assert!((cooldown.attenuate(8.0, at(500)) - 4.25).abs() < EPSILON);
    // Halfway through recovery the compression has faded halfway.
    assert!((cooldown.attenuate(8.0, at(1500)) - (4.25 + 3.75 * 0.5)).abs() < EPSILON);
    // Past recovery the burst runs at full magnitude again.
    assert!((cooldown.attenuate(8.0, at(2500)) - 8.0).abs() < EPSILON);
}

#[test]
fn quick_reversal_of_preaccelerated_input_restarts_cold() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // Hot downward stream, then an opposing tick 40 ms after its last tick:
    // the reversal passes only 40/400 of its pre-accelerated magnitude.
    for (millis, delta) in [(0, -5.0), (50, -5.0), (90, 5.0)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            preaccelerated(),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, -5.0 - 5.0 + 0.5));
    assert!(engine.active.is_empty());
}

#[test]
fn leisurely_reversal_skips_the_ramp_but_still_compresses() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // The opposing tick arrives a full cooldown after the departed
    // direction's last tick — already cold, so the ramp passes it whole —
    // but at 5 lines it exceeds the knee: 3 + 2/4 = 3.5 at burst start.
    for (millis, delta) in [(0, -5.0), (50, -5.0), (460, 5.0)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            preaccelerated(),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(700), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, -10.0 + 3.5));
    assert!(engine.active.is_empty());
}

#[test]
fn free_spin_jitter_never_scales_the_resumed_direction() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // A sub-commit opposing jitter tick is itself attenuated (0.2 × 30/400)
    // but must not cool the main direction when it resumes.
    for (millis, delta) in [(0, -5.0), (50, -5.0), (80, 0.2), (120, -5.0)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            preaccelerated(),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(400), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, -15.0 + 0.2 * 0.075));
    assert!(engine.active.is_empty());
}

#[test]
fn raw_input_reverses_without_attenuation() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // Non-pre-accelerated sources (HID++ divert, other platforms) keep the
    // exact conserve-net-distance reversal semantics.
    for (millis, delta) in [(0, -5.0), (8, 5.0)] {
        engine.impulse(
            source(),
            wheel(0.0, delta),
            base + Duration::from_millis(millis),
            neutral(),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, 0.0));
    assert!(engine.active.is_empty());
}

#[test]
fn sensitivity_scales_distance_but_not_acceleration() {
    // An OS hook at sensitivity 100 queues each notch as 100/14 lines.
    let scale = 100.0 / 14.0;
    let scaled = MotionTuning {
        distance_scale: wheel(1.0, scale),
        ..tuning(1.0, 100, 7.0)
    };

    // One isolated notch is still one notch of wheel rate: no gain.
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    engine.impulse(source(), wheel(0.0, scale), base, scaled, &mut |frame| {
        frames.push(frame);
    });
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, scale));

    // The fast ramp from `synthetic_fast_ticks_gain_amplitude_deterministically`
    // gains exactly as it does at the default sensitivity, scaled by it.
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    for millis in (0..=70).step_by(10) {
        engine.impulse(
            source(),
            wheel(0.0, scale),
            base + Duration::from_millis(millis),
            scaled,
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(300), &mut |frame| {
        frames.push(frame);
    });
    assert_delta(cumulative(&frames), wheel(0.0, scale * 169.0 / 14.0));
    assert!(engine.active.is_empty());
}

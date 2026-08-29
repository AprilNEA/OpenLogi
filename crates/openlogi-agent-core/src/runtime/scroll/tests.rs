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
    let gain = |millis, cap| accel_gain(Duration::from_millis(millis), cap);
    assert!((gain(10, 7.0) - 2.0).abs() < f64::EPSILON);
    assert!((gain(5, 7.0) - 3.5).abs() < f64::EPSILON);
    assert!((gain(20, 7.0) - 1.25).abs() < f64::EPSILON);
    assert!((gain(30, 7.0) - 1.0).abs() < f64::EPSILON);
    assert!(
        (gain(40, 7.0) - 1.0).abs() < f64::EPSILON,
        "sub-window slow ticks stay at 1×"
    );
    assert!(
        (gain(70, 10.0) - 1.0).abs() < f64::EPSILON,
        "window boundary is unaccelerated"
    );
    assert!((gain(1, 7.0) - 7.0).abs() < f64::EPSILON, "cap binds");
    assert!(
        (gain(0, 7.0) - 7.0).abs() < f64::EPSILON,
        "zero interval clamps to one millisecond"
    );
    assert!(
        (gain(1, 1.0) - 1.0).abs() < f64::EPSILON,
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
    // 10 ms spacing gains 2×; a 30 ms follow-up gains nothing.
    for millis in [0, 10, 40] {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_millis(millis),
            tuning(1.0, 100, 7.0),
            &mut |frame| frames.push(frame),
        );
    }
    engine.advance_due(base + Duration::from_millis(200), &mut |frame| {
        frames.push(frame);
    });

    assert_delta(cumulative(&frames), wheel(0.0, 1.0 + 2.0 + 1.0));
    assert!(engine.active.is_empty());
}

#[test]
fn synthetic_frame_interval_ticks_coalesce_and_cap_at_max_gain() {
    let base = Instant::now();
    let mut engine = ScrollEngine::default();
    let mut frames = Vec::new();
    // 1 ms spacing would gain 15.5×; the configured cap holds it at 7×, and
    // the tick lands inside the newest pulse instead of opening another.
    for millis in [0, 1] {
        engine.impulse(
            source(),
            wheel(0.0, 1.0),
            base + Duration::from_millis(millis),
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
        1
    );
    engine.advance_due(base + Duration::from_millis(200), &mut |frame| {
        frames.push(frame);
    });

    assert_delta(cumulative(&frames), wheel(0.0, 1.0 + 7.0));
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
    // Gains: 1× (first), 2× (10 ms), 2× (10 ms), 1× (30 ms) at step 3.
    assert_delta(cumulative(&frames), wheel(0.0, 3.0 * 6.0));
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

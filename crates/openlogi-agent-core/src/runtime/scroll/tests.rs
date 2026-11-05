//! Synthetic motion-model traces. These values are algorithm fixtures, not
//! measurements captured from physical hardware.

use std::ops::{Deref, DerefMut};

use openlogi_core::config::{GlideShape, MomentumParams, MomentumScrolling};

use super::*;

fn source() -> ScrollSource {
    ScrollSource::current_hook()
}

/// The main wheel of a HID++ capture session.
fn hidpp_source(device_key: &str, epoch: u64) -> ScrollSource {
    ScrollSource::Hidpp(
        HidppSessionId::with_epoch(device_key, epoch),
        HidppWheel::Main,
    )
}

/// The thumb wheel of a HID++ capture session.
fn thumb_source(device_key: &str, epoch: u64) -> ScrollSource {
    ScrollSource::Hidpp(
        HidppSessionId::with_epoch(device_key, epoch),
        HidppWheel::Thumb,
    )
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
        frames.iter().all(|f| f.kind == FrameKind::Phased),
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
    assert!(frames.iter().all(|f| f.kind == FrameKind::Phased));
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
    assert!(frames.iter().all(|f| f.kind != FrameKind::Phased));
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
        frames.iter().all(|f| f.kind == FrameKind::Phased),
        "the stream keeps the kind it opened with, so the gesture never changes kind"
    );
}

/// An engine whose smoothed main wheels all report one mouse's
/// momentum-scrolling settings, as the dispatcher passes them for it.
struct MomentumEngine {
    engine: ScrollEngine,
    momentum: Option<MomentumParams>,
}

impl MomentumEngine {
    fn new(momentum: MomentumScrolling) -> Self {
        Self {
            engine: ScrollEngine::default(),
            momentum: momentum.resolve(),
        }
    }

    /// One main-wheel impulse under this engine's settings.
    fn wheel_impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        notches: f64,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        self.engine
            .wheel_impulse(source, impulse, notches, self.momentum, at, emit);
    }
}

impl Deref for MomentumEngine {
    type Target = ScrollEngine;

    fn deref(&self) -> &ScrollEngine {
        &self.engine
    }
}

impl DerefMut for MomentumEngine {
    fn deref_mut(&mut self) -> &mut ScrollEngine {
        &mut self.engine
    }
}

/// An engine with momentum scrolling on at its default settings.
fn momentum_engine() -> MomentumEngine {
    MomentumEngine::new(MomentumScrolling {
        enabled: true,
        ..MomentumScrolling::default()
    })
}

#[test]
fn wheel_output_preserves_the_configured_distance_per_notch() {
    let defaults = MomentumScrolling::default();
    let baseline = f64::from(defaults.pixels_per_notch);
    for distance in [16_u16, defaults.pixels_per_notch, 52] {
        let mut engine = MomentumEngine::new(MomentumScrolling {
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
    engine: &mut MomentumEngine,
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
    engine: &mut MomentumEngine,
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
    assert!(
        frames.iter().all(|f| f.kind != FrameKind::Glide),
        "ratchet produced momentum"
    );
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
    let momentum = frames.iter().filter(|f| f.kind == FrameKind::Glide).count();
    assert!(momentum > 40, "only {momentum} momentum frames");
    assert!(
        frames
            .iter()
            .filter(|f| f.kind == FrameKind::Glide)
            .all(|f| f.delta.y > 0.0)
    );

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
        .filter(|(_, f)| f.kind == FrameKind::Glide)
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
    let mut engine = MomentumEngine::new(MomentumScrolling::default());
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
            .all(|f| f.kind != FrameKind::Glide && f.unit == FrameUnit::Ticks)
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
            .filter(|f| f.kind == FrameKind::Glide)
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

fn wheel_delta(notches: f64) -> WheelDelta {
    WheelDelta { x: 0.0, y: notches }
}

/// A ratchet reversal scrolls back smoothly: the page goes forward, turns
/// once and goes back, without lurching back and forth.
#[test]
fn a_confirmed_reversal_turns_the_page_around_once() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    // Ratchet ticks one way, then the other way soon after.
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

/// Only a wheel a finger stops mid-spin springs back: a one-notch
/// correction right after ratchet scrolling scrolls back, not held and
/// dropped as a bounce.
#[test]
fn a_ratchet_correction_right_after_scrolling_scrolls_back() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    let ticks: Vec<(u64, f64)> = vec![
        (0, 1.0),
        (40, 1.0),
        (80, 1.0),
        (120, 1.0),
        (160, 1.0),
        (220, -1.0),
    ];
    let mut back = 0.0_f64;
    for ms in 0..1200 {
        let at = start + Duration::from_millis(ms);
        let mut record = |f: ScrollFrame| back += in_ticks(&f).min(0.0);
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
    assert!(
        back < -0.9,
        "the correction scrolled {back:.2} notches back"
    );
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

/// A finger stopping a free-spinning wheel can make it spring back a step:
/// that step never scrolls the page backwards.
#[test]
fn a_spring_back_as_a_finger_stops_a_spin_is_dropped() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut forward = Vec::new();
    let stopped_at = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut forward,
    );
    assert!(
        forward.iter().any(|f| f.kind == FrameKind::Glide),
        "no glide to stop"
    );
    let mut after = Vec::new();
    let bounce_at = stopped_at + Duration::from_millis(50);
    run_on(
        &mut engine,
        stopped_at,
        Duration::from_millis(50),
        &mut after,
    );
    engine.wheel_impulse(source, wheel(0.0, -2.0), -1.0, bounce_at, &mut |f| {
        after.push(f);
    });
    run_on(&mut engine, bounce_at, Duration::from_secs(1), &mut after);
    assert!(
        after.iter().all(|f| f.delta.y >= 0.0),
        "the spring-back scrolled the page backwards"
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
    assert!(
        forward.iter().any(|f| f.kind == FrameKind::Glide),
        "no glide to reverse"
    );
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
    let held = back[..turned]
        .iter()
        .filter(|f| f.kind == FrameKind::Glide)
        .count();
    assert!(
        held <= 3,
        "the old glide ran {held} frames past the reversal"
    );
    assert!(
        back[turned..].iter().all(|f| f.delta.y <= 0.0),
        "the page went forward again after turning"
    );
}

/// An engine with momentum scrolling on, `change` applied to its defaults.
fn momentum_engine_with(change: impl FnOnce(&mut MomentumScrolling)) -> MomentumEngine {
    let mut momentum = MomentumScrolling {
        enabled: true,
        ..MomentumScrolling::default()
    };
    change(&mut momentum);
    MomentumEngine::new(momentum)
}

/// Run the engine's frames on from `from` for `duration`, a millisecond at a
/// time, with no wheel reports.
fn run_on(
    engine: &mut ScrollEngine,
    from: Instant,
    duration: Duration,
    frames: &mut Vec<ScrollFrame>,
) -> Instant {
    let mut at = from;
    while at < from + duration {
        engine.advance_due(at, &mut |f| frames.push(f));
        at += Duration::from_millis(1);
    }
    at
}

/// The glide's pixels across `frames`.
fn glide_pixels(frames: &[ScrollFrame]) -> f64 {
    frames
        .iter()
        .filter(|f| f.kind == FrameKind::Glide)
        .map(|f| f.delta.y)
        .sum()
}

/// The fastest glide frame across `frames`, px/s: frames are a frame
/// period apart, so each carries its speed times that period.
fn top_glide_speed(frames: &[ScrollFrame]) -> f64 {
    frames
        .iter()
        .filter(|f| f.kind == FrameKind::Glide)
        .map(|f| f.delta.y.abs() / FRAME_PERIOD.as_secs_f64())
        .fold(0.0, f64::max)
}

/// The wheel keeps spinning freely long after the glide it launched: the
/// glide follows its own curve, not the wheel, and every shape covers about
/// the same distance.
#[test]
fn every_glide_shape_covers_the_same_distance_under_a_spinning_wheel() {
    let mut distances = Vec::new();
    for shape in GlideShape::ALL {
        let mut engine = momentum_engine_with(|m| {
            m.glide_shape = shape;
            m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MAX;
        });
        let mut frames = Vec::new();
        drive_wheel(
            &mut engine,
            &hidpp_source("mouse", 1),
            Instant::now(),
            |_| 150.0,
            Duration::from_secs(4),
            &mut frames,
        );
        distances.push(glide_pixels(&frames));
    }
    let (low, high) = distances
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    assert!(low > 0.0, "{distances:?}");
    assert!(high / low < 1.05, "shapes glided apart: {distances:?}");
}

/// A glide covers its launch speed — the wheel's speed at the release, in
/// pixels and the user's sensitivity, times the flick response — times the
/// glide length, plus what it glided while the speed measure was still
/// rising toward the wheel's speed.
#[test]
fn a_glide_covers_its_launch_speed_times_the_glide_length() {
    let mut engine = momentum_engine_with(|m| {
        m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MAX;
    });
    let mut frames = Vec::new();
    drive_wheel(
        &mut engine,
        &hidpp_source("mouse", 1),
        Instant::now(),
        |_| 120.0,
        Duration::from_secs(4),
        &mut frames,
    );
    let params = MomentumScrolling {
        enabled: true,
        max_glide_speed: MomentumScrolling::MAX_GLIDE_SPEED_MAX,
        ..MomentumScrolling::default()
    }
    .resolve()
    .expect("on");
    // `drive_wheel` scrolls at 2× sensitivity.
    let launch = glide::launch_speed(120.0, 2.0, &params);
    let glided = glide_pixels(&frames);
    let (least, most) = (launch * 0.4 * 0.95, launch * (0.4 + 0.15) * 1.05);
    assert!(
        (least..=most).contains(&glided),
        "glided {glided:.0} px, expected {least:.0}–{most:.0}"
    );
}

/// Spin, catch the wheel, and spin again: within the repeat window the same
/// way, the second glide launches faster by the repeat speed-up; the other
/// way, or after the window, it launches as any spin does.
#[test]
fn a_repeated_spin_speeds_the_glide_up_only_the_same_way_and_soon() {
    let spin_twice = |direction: f64, gap: Duration| {
        let mut engine = momentum_engine_with(|m| {
            m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MAX;
            m.repeat_boost = 135;
        });
        let source = hidpp_source("mouse", 1);
        let mut first = Vec::new();
        let stopped = drive_wheel(
            &mut engine,
            &source,
            Instant::now(),
            |_| 120.0,
            Duration::from_millis(500),
            &mut first,
        );
        let resumed = run_on(&mut engine, stopped, gap, &mut first);
        let mut second = Vec::new();
        drive_wheel_toward(
            &mut engine,
            &source,
            resumed,
            direction,
            |_| 120.0,
            Duration::from_millis(500),
            &mut second,
        );
        top_glide_speed(&second) / top_glide_speed(&first)
    };
    let soon = Duration::from_millis(150);
    let repeated = spin_twice(1.0, soon);
    assert!(repeated > 1.25, "a repeat sped up only ×{repeated:.2}");
    let reversed = spin_twice(-1.0, soon);
    assert!(
        (reversed - 1.0).abs() < 0.1,
        "the other way sped up ×{reversed:.2}"
    );
    let late = spin_twice(1.0, Duration::from_secs(1));
    assert!(
        (late - 1.0).abs() < 0.1,
        "after the window sped up ×{late:.2}"
    );
}

/// One spin never glides faster than the max glide speed; however often the
/// wheel is spun again, repeats take it at most three times past it.
#[test]
fn spins_glide_no_faster_than_the_max_speed_and_repeats_three_times_it() {
    let mut engine = momentum_engine_with(|m| {
        m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MIN;
        m.repeat_boost = MomentumScrolling::REPEAT_BOOST_MAX;
    });
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    let mut at = Instant::now();
    for _ in 0..6 {
        at = drive_wheel(
            &mut engine,
            &source,
            at,
            |_| 250.0,
            Duration::from_millis(300),
            &mut frames,
        );
        at = run_on(&mut engine, at, Duration::from_millis(100), &mut frames);
    }
    let top = top_glide_speed(&frames);
    let max = f64::from(MomentumScrolling::MAX_GLIDE_SPEED_MIN);
    assert!(
        top > max * 1.5,
        "repeats never sped past one spin: {top:.0}"
    );
    assert!(
        top <= 3.0 * max * 1.01,
        "glided at {top:.0} px/s past 3 × {max}"
    );

    let mut engine = momentum_engine_with(|m| {
        m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MIN;
    });
    let mut one = Vec::new();
    drive_wheel(
        &mut engine,
        &hidpp_source("mouse", 1),
        Instant::now(),
        |_| 250.0,
        Duration::from_millis(300),
        &mut one,
    );
    let top = top_glide_speed(&one);
    assert!(
        top <= max * 1.01,
        "one spin glided at {top:.0} px/s past {max}"
    );
}

/// Each 60 Hz frame's glide speed, px/s, and when that glide launched
/// (`None` while no glide runs), as `reports` of `(µs, notches)` drive the
/// engine.
fn glide_speeds(
    engine: &mut MomentumEngine,
    reports: &[(Duration, f64)],
) -> Vec<Option<(f64, Instant)>> {
    let source = hidpp_source("mouse", 1);
    let start = Instant::now();
    let end = reports.last().map_or(Duration::ZERO, |r| r.0) + Duration::from_millis(300);
    let mut next = reports.iter().peekable();
    let frame = Duration::from_micros(16_667);
    let mut next_frame = frame;
    let mut t = Duration::ZERO;
    let mut speeds = Vec::new();
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
        engine.advance_due(at, &mut |_| {});
        if t >= next_frame {
            speeds.push(
                engine
                    .wheels
                    .get(&source)
                    .and_then(WheelState::gliding)
                    .map(|glide| (glide.speed(at), glide.launched)),
            );
            next_frame += frame;
        }
        t += Duration::from_millis(1);
    }
    speeds
}

/// The recorded series of relaxed repeated flicks (`repeat_flicks_recording.txt`),
/// replayed: within a glide every rise of its speed — the measure of a
/// speeding-up spin, a repeat's speed-up — is a ramp over several frames,
/// never a step. A rise taken in one frame snaps the page forward.
#[test]
fn recorded_repeated_flicks_glide_without_jumps() {
    let reports: Vec<(Duration, f64)> = include_str!("repeat_flicks_recording.txt")
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
    let speeds = glide_speeds(&mut momentum_engine(), &reports);
    let glides = speeds
        .windows(2)
        .filter(|w| {
            matches!((w[0], w[1]), (None, Some(_)))
                || matches!((w[0], w[1]), (Some((_, a)), Some((_, b))) if a != b)
        })
        .count();
    assert!(glides >= 5, "only {glides} glides in five series of flicks");
    // A run of rising frames: where it starts, its first speed, and its
    // biggest single-frame rise. No single frame may carry most of a run
    // that rose by a good share.
    let mut jumps = Vec::new();
    let mut run: Option<(usize, f64, f64)> = None;
    for (i, w) in speeds.windows(2).enumerate() {
        let rise = match (w[0], w[1]) {
            (Some((before, launched)), Some((after, same)))
                if launched == same && after > before =>
            {
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
                let total = w[0].map_or(from, |(speed, _)| speed) - from;
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

/// A glide eases up from the pace the page was scrolling at to its launch
/// speed, and a relaunch from the speed it is at — ~90% in within 70 ms,
/// never in one frame — and covers its curve's distance less what easing in
/// cost.
#[test]
fn a_glide_eases_up_into_its_curve_instead_of_jumping() {
    let momentum = MomentumScrolling {
        enabled: true,
        max_glide_speed: MomentumScrolling::MAX_GLIDE_SPEED_MAX,
        ..MomentumScrolling::default()
    }
    .resolve()
    .expect("on");
    let at = Instant::now();
    let ms = |n| at + Duration::from_millis(n);
    let mut g = Glide::new(Direction::Up, 1.0, 1.0, (2000.0, 6000.0), &momentum, at);
    assert!((g.speed(at) - 2000.0).abs() < 1e-9, "starts at the pace");
    let one_frame = g.speed(ms(8));
    assert!(one_frame < 3600.0, "{one_frame:.0} px/s after one frame");
    let curve = g.curve.speed_at(Duration::from_millis(70));
    assert!(g.speed(ms(70)) > 2000.0 + 0.85 * (curve - 2000.0));
    // A relaunch eases up from where it is, not from the curve's start.
    let before = g.speed(ms(100));
    g.relaunch(12000.0, &momentum, ms(100));
    assert!((g.speed(ms(100)) - before).abs() < 1e-9);
    assert!(g.speed(ms(108)) < before + 0.5 * (12000.0 - before));
    // It covers its curve's distance, less what easing in cost.
    let shortfall = g.rise * glide::RISE_S;
    let end = ms(100) + g.curve.duration();
    assert!((g.position(end) - (g.curve.distance() - shortfall)).abs() < 1.0);
}

/// A wheel flicked again every `gap` seconds: each flick spins it up to 220
/// notches/s, and it slows toward 120 until the next — the finger never lets
/// it stop.
fn repeated_flicks(t: f64, flicks: u32, gap: f64) -> f64 {
    let index = (t / gap).floor().min(f64::from(flicks - 1));
    let since = t - index * gap;
    (220.0 - 250.0 * since).max(120.0)
}

/// `flicks` repeated flicks at `repeat_boost`: the glide's pixels, and
/// whether any frame fell back to ratchet output after the first glide began.
fn glide_with_chain(repeat_boost: u16, flicks: u32) -> (f64, bool) {
    let mut engine = momentum_engine_with(|m| {
        m.repeat_boost = repeat_boost;
        m.max_glide_speed = MomentumScrolling::MAX_GLIDE_SPEED_MAX;
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
    let first_glide = frames
        .iter()
        .position(|f| f.kind == FrameKind::Glide)
        .unwrap_or(frames.len());
    let fell_back = frames[first_glide..]
        .iter()
        .any(|f| f.kind != FrameKind::Glide && f.delta.y != 0.0);
    (glide_pixels(&frames), fell_back)
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

/// The worker sleeps until the engine's next deadline. While a glide runs,
/// stepping to that deadline must always leave the next one in the future;
/// an expired deadline would spin the worker on zero timeouts.
#[test]
fn a_glide_never_leaves_an_expired_deadline_behind() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut at = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(400),
        &mut Vec::new(),
    );
    assert!(engine.next_deadline().is_some(), "no glide to step");
    let mut steps = 0;
    while let Some(deadline) = engine.next_deadline() {
        assert!(deadline >= at, "deadline {:?} behind", at - deadline);
        at = deadline;
        engine.advance_due(at, &mut |_| {});
        if let Some(next) = engine.next_deadline() {
            assert!(next > at, "the deadline did not move past {at:?}");
        }
        steps += 1;
        assert!(steps < 10_000, "the glide never ended");
    }
}

/// With momentum scrolling on, the main wheel's momentum stays its own: the
/// thumb wheel of the same session keeps easing in ticks, and a main-wheel
/// glide starting does not drop the thumb's unfinished motion.
#[test]
fn momentum_on_the_main_wheel_leaves_the_thumb_wheel_alone() {
    let mut engine = momentum_engine();
    let thumb = thumb_source("mouse", 1);
    let start = Instant::now();
    let mut frames = Vec::new();
    // A main-wheel tick registers the wheel for momentum…
    engine.wheel_impulse(
        hidpp_source("mouse", 1),
        wheel(0.0, 1.0),
        1.0,
        start,
        &mut |f| frames.push(f),
    );
    // …then the thumb scrolls while the main wheel spins up into a glide.
    let at = start + Duration::from_millis(20);
    engine.impulse(thumb, wheel(3.0, 0.0), at, &mut |f| frames.push(f));
    drive_wheel(
        &mut engine,
        &hidpp_source("mouse", 1),
        at,
        |_| 200.0,
        Duration::from_millis(400),
        &mut frames,
    );
    assert!(
        frames.iter().any(|f| f.kind == FrameKind::Glide),
        "the main wheel never glided"
    );
    let thumb_frames: Vec<&ScrollFrame> = frames.iter().filter(|f| f.delta.x != 0.0).collect();
    assert!(
        thumb_frames
            .iter()
            .all(|f| f.unit == FrameUnit::Ticks && f.kind == FrameKind::Eased),
        "the thumb wheel took the main wheel's momentum output"
    );
    let thumb_total: f64 = thumb_frames.iter().map(|f| f.delta.x).sum();
    assert!(
        (thumb_total - 3.0).abs() < 1e-9,
        "the thumb wheel scrolled {thumb_total} of 3 ticks"
    );
}

/// A session ending cancels both of its wheels, and another session's
/// motion carries on.
#[test]
fn cancelling_a_session_cancels_both_its_wheels() {
    let mut engine = momentum_engine();
    let start = Instant::now();
    let mut frames = Vec::new();
    engine.wheel_impulse(
        hidpp_source("mouse", 1),
        wheel(0.0, 1.0),
        1.0,
        start,
        &mut |f| {
            frames.push(f);
        },
    );
    engine.impulse(thumb_source("mouse", 1), wheel(2.0, 0.0), start, &mut |f| {
        frames.push(f);
    });
    engine.impulse(thumb_source("other", 1), wheel(2.0, 0.0), start, &mut |f| {
        frames.push(f);
    });
    engine.cancel_session(&HidppSessionId::with_epoch("mouse", 1), &mut |f| {
        frames.push(f);
    });
    assert!(!engine.active.contains_key(&hidpp_source("mouse", 1)));
    assert!(!engine.active.contains_key(&thumb_source("mouse", 1)));
    assert!(!engine.wheels.contains_key(&hidpp_source("mouse", 1)));
    assert!(engine.active.contains_key(&thumb_source("other", 1)));
}

/// With smoothing off the thumb wheel scrolls in a phased gesture. Main-wheel
/// steps under momentum scrolling stay out of it: phaseless pixels at the
/// configured distance, whether or not the thumb just moved — and the
/// thumb's gesture still ends with its own phase.
#[test]
fn momentum_steps_stay_out_of_the_thumb_wheels_phased_gesture() {
    let mut engine = momentum_engine();
    let start = Instant::now();
    let mut frames = Vec::new();
    engine.phased_impulse(thumb_source("mouse", 1), wheel(1.0, 0.0), start, &mut |f| {
        frames.push(f);
    });
    engine.wheel_impulse(
        hidpp_source("mouse", 1),
        wheel(0.0, 1.0),
        1.0,
        start + Duration::from_millis(10),
        &mut |f| frames.push(f),
    );
    let mut at = start;
    while at < start + Duration::from_millis(400) {
        engine.advance_due(at, &mut |f| frames.push(f));
        at += Duration::from_millis(1);
    }
    let main: Vec<&ScrollFrame> = frames.iter().filter(|f| f.delta.y != 0.0).collect();
    assert!(!main.is_empty());
    assert!(
        main.iter().all(|f| f.kind == FrameKind::Eased
            && f.unit
                == FrameUnit::Pixels {
                    per_tick: f64::from(MomentumScrolling::default().pixels_per_notch)
                }),
        "a main-wheel step joined the thumb's phased gesture: {main:?}"
    );
    let px: f64 = main.iter().map(|f| f.delta.y).sum();
    let per_notch = f64::from(MomentumScrolling::default().pixels_per_notch);
    assert!(
        (px - per_notch).abs() < 1e-9,
        "scrolled {px} px of {per_notch}"
    );
    let thumb: Vec<SmoothScrollPhase> = frames
        .iter()
        .filter(|f| f.kind == FrameKind::Phased)
        .map(|f| f.phase)
        .collect();
    assert_eq!(thumb.first(), Some(&SmoothScrollPhase::Began));
    assert_eq!(thumb.last(), Some(&SmoothScrollPhase::Ended));
}

/// A glide runs out while the wheel still spins freely; a finger then
/// catches the wheel and scrolls on slowly the same way. The silence of the
/// catch must still read as a stop, so those slow strokes scroll instead of
/// being swallowed as more of the spent spin.
#[test]
fn slow_strokes_after_a_spent_glide_and_a_catch_still_scroll() {
    let mut engine = momentum_engine();
    let source = hidpp_source("mouse", 1);
    let mut frames = Vec::new();
    // Spinning on well past the default glide's 1.2 s.
    let caught = drive_wheel(
        &mut engine,
        &source,
        Instant::now(),
        |_| 200.0,
        Duration::from_millis(2500),
        &mut frames,
    );
    assert!(
        matches!(
            engine.wheels.get(&source).map(|s| &s.glide),
            Some(GlideState::Spent { .. })
        ),
        "the glide did not run out under the spinning wheel"
    );
    // The finger holds the wheel still for 150 ms…
    let resumed = run_on(&mut engine, caught, Duration::from_millis(150), &mut frames);
    // …then scrolls one notch every 70 ms, the same way.
    let mut strokes = Vec::new();
    let mut at = resumed;
    for _ in 0..5 {
        engine.wheel_impulse(source.clone(), wheel(0.0, 1.0), 1.0, at, &mut |f| {
            strokes.push(f);
        });
        at = run_on(&mut engine, at, Duration::from_millis(70), &mut strokes);
    }
    let scrolled: f64 = strokes
        .iter()
        .filter(|f| f.kind != FrameKind::Glide)
        .map(in_ticks)
        .sum();
    assert!(
        (scrolled - 5.0).abs() < 1e-6,
        "only {scrolled} of 5 slow notches scrolled after the catch"
    );
}

/// Each wheel follows the settings its own reports carry: a mouse with
/// momentum scrolling off keeps easing in ticks beside one that glides.
#[test]
fn each_wheel_follows_its_own_devices_momentum_settings() {
    let mut engine = ScrollEngine::default();
    let gliding = hidpp_source("mouse", 1);
    let plain = hidpp_source("other", 1);
    let on = MomentumScrolling {
        enabled: true,
        ..MomentumScrolling::default()
    }
    .resolve();
    let start = Instant::now();
    let mut frames = Vec::new();
    engine.wheel_impulse(gliding.clone(), wheel(0.0, 1.0), 1.0, on, start, &mut |f| {
        frames.push(f);
    });
    engine.wheel_impulse(plain.clone(), wheel(0.0, 1.0), 1.0, None, start, &mut |f| {
        frames.push(f);
    });
    assert!(engine.wheels.contains_key(&gliding));
    assert!(!engine.wheels.contains_key(&plain));
    assert!(engine.pixels_for(&gliding).is_some());
    assert_eq!(engine.pixels_for(&plain), None);
}

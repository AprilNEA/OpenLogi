//! Boundary behaviour of the Ctrl+wheel quantizer.

use std::time::{Duration, Instant};

use super::super::BUTTON_ZOOM_SOURCE;
use super::{IDLE, ZoomNotchRegistry, ZoomNotches};

/// One notch is worth this much magnification on both quantized platforms.
const PER_NOTCH: f64 = 0.05;

#[test]
fn fractions_accumulate_into_whole_notches() {
    let mut q = ZoomNotches::new();
    let t = Instant::now();

    // A third of a notch at a time: nothing, nothing, then one.
    assert_eq!(q.take(PER_NOTCH / 3.0, PER_NOTCH, t), 0);
    assert_eq!(q.take(PER_NOTCH / 3.0, PER_NOTCH, t), 0);
    assert_eq!(q.take(PER_NOTCH / 3.0, PER_NOTCH, t), 1);
}

/// The bug this type exists to prevent: leftover zoom-in progress must not be
/// spent against the zoom-out the user just asked for.
#[test]
fn reversing_direction_does_not_spend_the_leftover_against_the_new_one() {
    let mut q = ZoomNotches::new();
    let t = Instant::now();

    // Leave 0.9 of a notch of zoom-in progress behind.
    assert_eq!(q.take(PER_NOTCH * 0.9, PER_NOTCH, t), 0);

    // Reversing immediately: a full notch out must zoom out *now*. Carrying
    // the +0.9 would make this 0.9 - 1.0 = -0.1 and emit nothing.
    assert_eq!(
        q.take(-PER_NOTCH, PER_NOTCH, t),
        -1,
        "a full notch in the new direction must land on the first step"
    );
}

#[test]
fn a_pause_longer_than_the_idle_window_starts_a_fresh_gesture() {
    let mut q = ZoomNotches::new();
    let t = Instant::now();

    assert_eq!(q.take(PER_NOTCH * 0.9, PER_NOTCH, t), 0);

    // Same direction, but long after the turn stopped — this is a new gesture,
    // possibly a different device or binding, so it starts from zero.
    let later = t + IDLE + Duration::from_millis(1);
    assert_eq!(
        q.take(PER_NOTCH * 0.9, PER_NOTCH, later),
        0,
        "stale progress must not complete a notch for an unrelated later turn"
    );
}

#[test]
fn an_uninterrupted_turn_keeps_its_progress() {
    let mut q = ZoomNotches::new();
    let t = Instant::now();

    assert_eq!(q.take(PER_NOTCH * 0.6, PER_NOTCH, t), 0);
    // Still the same direction, still inside the idle window: the fraction is
    // real progress and must count.
    let soon = t + IDLE / 2;
    assert_eq!(
        q.take(PER_NOTCH * 0.6, PER_NOTCH, soon),
        1,
        "progress within one continuous turn must accumulate"
    );
}

#[test]
fn a_step_larger_than_one_notch_emits_every_notch_it_covers() {
    let mut q = ZoomNotches::new();
    assert_eq!(q.take(PER_NOTCH * 3.5, PER_NOTCH, Instant::now()), 3);
}

/// The defect the registry exists to prevent: two mice zooming at once, or a
/// button-bound zoom next to a wheel-bound one, must not spend each other's
/// progress.
#[test]
fn one_source_does_not_consume_another_sources_progress() {
    let mut reg = ZoomNotchRegistry::new();
    let t = Instant::now();

    // Mouse A leaves 0.9 of a notch behind.
    assert_eq!(reg.take("mouse-a", PER_NOTCH * 0.9, PER_NOTCH, t), 0);

    // Mouse B's first tenth of a notch must not ride A's leftover into a notch.
    assert_eq!(
        reg.take("mouse-b", PER_NOTCH * 0.1, PER_NOTCH, t),
        0,
        "a second device must start from zero, not from another device's progress"
    );

    // And A's own progress is still intact: one more tenth completes its notch.
    assert_eq!(
        reg.take("mouse-a", PER_NOTCH * 0.1, PER_NOTCH, t),
        1,
        "the first device's progress must survive the second device's step"
    );
}

#[test]
fn a_button_is_a_separate_source_from_the_wheel() {
    let mut reg = ZoomNotchRegistry::new();
    let t = Instant::now();

    assert_eq!(reg.take("mouse-a", PER_NOTCH * 0.9, PER_NOTCH, t), 0);
    assert_eq!(
        reg.take(BUTTON_ZOOM_SOURCE, PER_NOTCH * 0.9, PER_NOTCH, t),
        0,
        "a button press must not finish a notch the wheel was halfway through"
    );
}

/// Devices unplug and bindings change, so the map must not grow forever.
#[test]
fn quiet_sources_are_dropped_but_the_active_one_is_kept() {
    let mut reg = ZoomNotchRegistry::new();
    let t = Instant::now();

    reg.take("gone", PER_NOTCH * 0.5, PER_NOTCH, t);
    assert_eq!(reg.len(), 1);

    // Long after "gone" fell silent, another source steps.
    let later = t + IDLE + Duration::from_millis(1);
    reg.take("mouse-a", PER_NOTCH * 0.5, PER_NOTCH, later);
    assert_eq!(
        reg.len(),
        1,
        "the silent source is dropped and only the active one remains"
    );

    // The survivor is the active source, with its progress intact.
    assert_eq!(reg.take("mouse-a", PER_NOTCH * 0.5, PER_NOTCH, later), 1);
}

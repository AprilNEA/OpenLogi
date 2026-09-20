//! Thumb-wheel state-transition tests.

use super::*;

/// The resolutions traced off an MX Master 4 over Bolt: 20 ratchets per
/// revolution natively, 120 increments per revolution diverted.
const TRACED: WheelResolution = WheelResolution {
    native_res: 20,
    diverted_res: 120,
};
const UNSCALED: WheelResolution = WheelResolution {
    native_res: 1,
    diverted_res: 1,
};

fn unscaled(sensitivity: ThumbwheelSensitivity) -> ScrollScale {
    ScrollScale::new(UNSCALED, sensitivity)
}

fn scroll_delta(output: WheelOutput) -> ScrollDelta {
    let WheelOutput::Scroll(delta) = output else {
        panic!("expected fractional scroll output");
    };
    delta
}

fn assert_distance(actual: f64, expected: f64) {
    const EPSILON: f64 = 1.0e-12;
    assert!(
        (actual - expected).abs() < EPSILON,
        "{actual} != {expected}"
    );
}

#[test]
fn multiplier_is_unity_at_default_sensitivity() {
    assert!((unscaled(ThumbwheelSensitivity::DEFAULT).per_increment() - 1.0).abs() < f64::EPSILON);
    assert!(unscaled(ThumbwheelSensitivity::from_rounded(28.0)).per_increment() > 1.9);
    assert!(unscaled(ThumbwheelSensitivity::MIN).per_increment() < 0.1);
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

#[test]
fn an_unreported_resolution_leaves_increments_unscaled() {
    let scale = ScrollScale::new(WheelResolution::UNKNOWN, ThumbwheelSensitivity::DEFAULT);
    assert!((scale.per_increment() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn rotation_separates_direction_and_positive_magnitude() {
    let up = WheelRotation::from_increments(3).expect("non-zero rotation");
    assert_eq!(up.button(), ButtonId::ThumbwheelScrollUp);
    assert_eq!(up.magnitude, 3);

    let down = WheelRotation::from_increments(-3).expect("non-zero rotation");
    assert_eq!(down.button(), ButtonId::ThumbwheelScrollDown);
    assert_eq!(down.magnitude, 3);
    assert_eq!(WheelRotation::from_increments(0), None);
}

#[test]
fn a_revolution_scrolls_its_native_amount_however_finely_the_wheel_reports() {
    let scale = ScrollScale::new(TRACED, ThumbwheelSensitivity::DEFAULT);
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let mut distance = 0.0;
    for _ in 0..120 {
        distance +=
            scroll_delta(direction.advance(&Action::HorizontalScrollRight, 1, scale, now)).x();
    }
    assert_distance(distance, 20.0);
}

#[test]
fn sensitivity_multiplies_the_native_amount() {
    let scale = ScrollScale::new(TRACED, ThumbwheelSensitivity::from_rounded(28.0));
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let mut distance = 0.0;
    for _ in 0..120 {
        distance +=
            scroll_delta(direction.advance(&Action::HorizontalScrollRight, 1, scale, now)).x();
    }
    assert_distance(distance, 40.0);
}

#[test]
fn sub_tick_distance_is_emitted_without_integer_accumulation() {
    let output = WheelDirection::default().advance(
        &Action::HorizontalScrollRight,
        1,
        unscaled(ThumbwheelSensitivity::from_rounded(7.0)),
        Instant::now(),
    );
    assert_eq!(
        output,
        WheelOutput::Scroll(ScrollDelta::wheel_ticks(0.5, 0.0))
    );
}

#[test]
fn all_scroll_bindings_emit_on_their_configured_axis_and_sign() {
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    for (action, expected) in [
        (Action::ScrollUp, ScrollDelta::wheel_ticks(0.0, 1.0)),
        (Action::ScrollDown, ScrollDelta::wheel_ticks(0.0, -1.0)),
        (
            Action::HorizontalScrollRight,
            ScrollDelta::wheel_ticks(1.0, 0.0),
        ),
        (
            Action::HorizontalScrollLeft,
            ScrollDelta::wheel_ticks(-1.0, 0.0),
        ),
    ] {
        assert_eq!(
            WheelDirection::default().advance(&action, 1, scale, now),
            WheelOutput::Scroll(expected)
        );
    }
}

#[test]
fn scroll_scale_changes_apply_only_to_the_current_delta() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let traced = ScrollScale::new(TRACED, ThumbwheelSensitivity::DEFAULT);

    assert_distance(
        scroll_delta(direction.advance(&Action::HorizontalScrollRight, 1, traced, now)).x(),
        1.0 / 6.0,
    );
    assert_eq!(
        direction.advance(
            &Action::HorizontalScrollRight,
            1,
            unscaled(ThumbwheelSensitivity::DEFAULT),
            now,
        ),
        WheelOutput::Scroll(ScrollDelta::wheel_ticks(1.0, 0.0)),
        "the new resolution must not combine with hidden old-scale progress"
    );
}

#[test]
fn physical_directions_accumulate_independently() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    let up = WheelRotation::from_increments(1).expect("non-zero rotation");
    let down = WheelRotation::from_increments(-1).expect("non-zero rotation");

    assert_eq!(
        wheel.advance(up, &Action::VolumeUp, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        wheel.advance(down, &Action::VolumeDown, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        wheel.advance(
            WheelRotation {
                magnitude: threshold - 1,
                ..up
            },
            &Action::VolumeUp,
            scale,
            now,
        ),
        WheelOutput::FireAction
    );
    assert_eq!(
        wheel.advance(
            WheelRotation {
                magnitude: threshold - 1,
                ..down
            },
            &Action::VolumeDown,
            scale,
            now,
        ),
        WheelOutput::FireAction
    );
}

#[test]
fn custom_action_fires_on_threshold_then_respects_its_cooldown() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold, scale, now),
        WheelOutput::FireAction
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold, scale, now + ACTION_COOLDOWN),
        WheelOutput::FireAction,
        "the same action may fire again at the cooldown boundary"
    );
}

#[test]
fn custom_binding_changes_discard_progress_and_cooldown() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::NextTab, 1, scale, now),
        WheelOutput::Idle,
        "the new action must not inherit the previous action's progress"
    );
    assert_eq!(
        direction.advance(&Action::NextTab, threshold - 1, scale, now),
        WheelOutput::FireAction
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold, scale, now),
        WheelOutput::FireAction,
        "the new binding must not inherit another action's cooldown"
    );
}

#[test]
fn sensitivity_changes_discard_progress_and_cooldown() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let default = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, default, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(
            &Action::VolumeUp,
            1,
            unscaled(ThumbwheelSensitivity::MIN),
            now,
        ),
        WheelOutput::Idle,
        "a new threshold must not inherit progress earned under the old one"
    );

    assert_eq!(
        direction.advance(
            &Action::VolumeUp,
            ThumbwheelSensitivity::MIN.action_threshold(),
            unscaled(ThumbwheelSensitivity::MIN),
            now,
        ),
        WheelOutput::FireAction
    );
    assert_eq!(
        direction.advance(
            &Action::VolumeUp,
            ThumbwheelSensitivity::MAX.action_threshold(),
            unscaled(ThumbwheelSensitivity::MAX),
            now,
        ),
        WheelOutput::FireAction,
        "a sensitivity change must not inherit the old threshold's cooldown"
    );
}

#[test]
fn changing_modes_discards_discrete_progress() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::HorizontalScrollRight, 1, scale, now),
        WheelOutput::Scroll(ScrollDelta::wheel_ticks(1.0, 0.0))
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, 1, scale, now),
        WheelOutput::Idle,
        "returning to an action must not recover progress from before scrolling"
    );
}

#[test]
fn none_suppresses_input_and_clears_retained_progress() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::None, 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, 1, scale, now),
        WheelOutput::Idle
    );
}

#[test]
fn stale_custom_progress_decays() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    let after_decay = now + ACTION_DECAY + Duration::from_millis(1);
    assert_eq!(
        direction.advance(&Action::VolumeUp, 1, scale, after_decay),
        WheelOutput::Idle,
        "the stale partial action must have been discarded"
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, after_decay),
        WheelOutput::FireAction
    );
}

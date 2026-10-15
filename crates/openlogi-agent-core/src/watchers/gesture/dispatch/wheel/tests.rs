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
        WheelOutput::FireAction(1)
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
        WheelOutput::FireAction(1)
    );
}

#[test]
fn custom_action_fires_on_threshold_then_respects_its_cooldown() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::NextTab, threshold, scale, now),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        direction.advance(&Action::NextTab, threshold, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::NextTab, threshold, scale, now + ACTION_COOLDOWN),
        WheelOutput::FireAction(1),
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
        direction.advance(&Action::NextTab, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::PrevTab, 1, scale, now),
        WheelOutput::Idle,
        "the new action must not inherit the previous action's progress"
    );
    assert_eq!(
        direction.advance(&Action::PrevTab, threshold - 1, scale, now),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        direction.advance(&Action::NextTab, threshold, scale, now),
        WheelOutput::FireAction(1),
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
        direction.advance(&Action::NextTab, threshold - 1, default, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(
            &Action::NextTab,
            1,
            unscaled(ThumbwheelSensitivity::MIN),
            now,
        ),
        WheelOutput::Idle,
        "a new threshold must not inherit progress earned under the old one"
    );

    assert_eq!(
        direction.advance(
            &Action::NextTab,
            ThumbwheelSensitivity::MIN.action_threshold(),
            unscaled(ThumbwheelSensitivity::MIN),
            now,
        ),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        direction.advance(
            &Action::NextTab,
            ThumbwheelSensitivity::MAX.action_threshold(),
            unscaled(ThumbwheelSensitivity::MAX),
            now,
        ),
        WheelOutput::FireAction(1),
        "a sensitivity change must not inherit the old threshold's cooldown"
    );
}

#[test]
fn a_repeatable_action_fires_once_per_threshold_crossed_in_one_swipe() {
    // Volume bindings repeat across thresholds and bypass the discrete cooldown.
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold * 3, scale, now),
        WheelOutput::FireAction(3),
        "a swipe spanning three thresholds at once must fire three times"
    );
    // The very next event, an instant later, still fires — no cooldown gate.
    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold, scale, now),
        WheelOutput::FireAction(1),
        "a repeatable action must not be cooldown-gated like an ordinary one"
    );
}

#[test]
fn a_single_event_never_fires_more_than_the_repeat_cap() {
    // A maximum report at threshold 1 must still respect the per-event cap.
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::MAX);
    assert_eq!(ThumbwheelSensitivity::MAX.action_threshold(), 1);

    assert_eq!(
        direction.advance(&Action::VolumeUp, i32::from(i16::MAX), scale, now),
        WheelOutput::FireAction(MAX_REPEATS_PER_EVENT.cast_unsigned())
    );
}

#[test]
fn a_capped_event_discards_its_excess_distance_instead_of_carrying_it_over() {
    // Distance discarded by the cap must not create another burst on a later nudge.
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::MAX);
    assert_eq!(ThumbwheelSensitivity::MAX.action_threshold(), 1);

    assert_eq!(
        direction.advance(&Action::VolumeUp, i32::from(i16::MAX), scale, now),
        WheelOutput::FireAction(MAX_REPEATS_PER_EVENT.cast_unsigned())
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, 1, scale, now),
        WheelOutput::FireAction(1),
        "one more small nudge must fire once, not re-trigger another capped burst \
         from the previous report's leftover distance"
    );
}

#[test]
fn repeatable_action_progress_below_threshold_carries_over() {
    let mut direction = WheelDirection::default();
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();

    assert_eq!(
        direction.advance(&Action::VolumeUp, threshold - 1, scale, now),
        WheelOutput::Idle
    );
    assert_eq!(
        direction.advance(&Action::VolumeUp, 1, scale, now),
        WheelOutput::FireAction(1),
        "the carried-over increment plus one more must cross the threshold"
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
        WheelOutput::FireAction(1)
    );
}

fn zoom_report(
    wheel: &mut WheelAccumulators,
    increments: i16,
    sensitivity: ThumbwheelSensitivity,
    now: Instant,
) -> WheelOutput {
    let Some(rotation) = WheelRotation::from_increments(increments) else {
        return WheelOutput::Idle;
    };
    wheel.advance_zoom(rotation, sensitivity, now)
}

#[test]
fn zoom_recognition_discards_first_report_and_requires_strict_threshold() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    assert_eq!(
        zoom_report(&mut wheel, 100, sensitivity, now),
        WheelOutput::Idle
    );
    // Default threshold is six. Six unit reports must not fire.
    for _ in 0..6 {
        assert_eq!(
            zoom_report(&mut wheel, 1, sensitivity, now),
            WheelOutput::Idle
        );
    }
    assert_eq!(
        zoom_report(&mut wheel, 1, sensitivity, now),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_nonlinear_truncation_applies_per_report_in_each_direction() {
    let now = Instant::now();
    for sign in [-1, 1] {
        let mut wheel = WheelAccumulators::default();
        let sensitivity = ThumbwheelSensitivity::DEFAULT;
        assert_eq!(
            zoom_report(&mut wheel, sign, sensitivity, now),
            WheelOutput::Idle
        );
        // |3|^1.4 truncates to four; |2|^1.4 truncates to two. Their
        // six total must stay below the strict boundary, unlike grouped input.
        assert_eq!(
            zoom_report(&mut wheel, 3 * sign, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut wheel, 2 * sign, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut wheel, sign, sensitivity, now),
            WheelOutput::FireAction(1)
        );
        let mut grouped = WheelAccumulators::default();
        assert_eq!(
            zoom_report(&mut grouped, sign, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut grouped, 5 * sign, sensitivity, now),
            WheelOutput::FireAction(1)
        );
    }
}

#[test]
fn zoom_large_reports_fire_once_discard_overshoot_and_have_no_cooldown() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    assert_eq!(
        zoom_report(&mut wheel, i16::MIN, sensitivity, now),
        WheelOutput::Idle
    );
    assert_eq!(
        zoom_report(&mut wheel, i16::MIN, sensitivity, now),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        zoom_report(&mut wheel, -1, sensitivity, now),
        WheelOutput::Idle
    );
    assert_eq!(
        zoom_report(&mut wheel, -100, sensitivity, now),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        zoom_report(&mut wheel, -100, sensitivity, now),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_reversal_cancels_signed_progress_before_firing() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    assert_eq!(
        zoom_report(&mut wheel, 1, sensitivity, now),
        WheelOutput::Idle
    );
    assert_eq!(
        zoom_report(&mut wheel, 4, sensitivity, now),
        WheelOutput::Idle
    ); // +6
    assert_eq!(
        zoom_report(&mut wheel, -4, sensitivity, now),
        WheelOutput::Idle
    ); // 0
    assert_eq!(
        zoom_report(&mut wheel, -4, sensitivity, now),
        WheelOutput::Idle
    ); // -6
    assert_eq!(
        zoom_report(&mut wheel, -1, sensitivity, now),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_slider_normalization_preserves_default_and_endpoints() {
    let now = Instant::now();
    for (sensitivity, threshold) in [
        (ThumbwheelSensitivity::MIN, 10),
        (ThumbwheelSensitivity::DEFAULT, 6),
        (ThumbwheelSensitivity::MAX, 2),
        (ThumbwheelSensitivity::from_rounded(57.0), 4),
    ] {
        let mut wheel = WheelAccumulators::default();
        assert_eq!(
            zoom_report(&mut wheel, 100, sensitivity, now),
            WheelOutput::Idle
        );
        for _ in 0..threshold {
            assert_eq!(
                zoom_report(&mut wheel, 1, sensitivity, now),
                WheelOutput::Idle
            );
        }
        assert_eq!(
            zoom_report(&mut wheel, 1, sensitivity, now),
            WheelOutput::FireAction(1)
        );
    }
}

#[test]
fn zoom_inactivity_resets_only_beyond_five_hundred_milliseconds() {
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    for (gap, expected) in [(500, WheelOutput::FireAction(1)), (501, WheelOutput::Idle)] {
        let mut wheel = WheelAccumulators::default();
        assert_eq!(
            zoom_report(&mut wheel, 1, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut wheel, 4, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut wheel, 1, sensitivity, now + Duration::from_millis(gap)),
            expected
        );
    }
}

#[test]
fn zoom_zero_progress_never_fires_or_refreshes_inactivity() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    assert_eq!(
        zoom_report(&mut wheel, 1, sensitivity, now),
        WheelOutput::Idle
    );
    assert_eq!(
        zoom_report(&mut wheel, 4, sensitivity, now),
        WheelOutput::Idle
    );
    for offset in [100, 300, 500] {
        assert_eq!(
            zoom_report(
                &mut wheel,
                0,
                sensitivity,
                now + Duration::from_millis(offset)
            ),
            WheelOutput::Idle
        );
    }
    assert_eq!(
        zoom_report(&mut wheel, 1, sensitivity, now + Duration::from_millis(501)),
        WheelOutput::Idle
    );
}

#[test]
fn zoom_sensitivity_changes_discard_recognition_and_displacement() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let default = ThumbwheelSensitivity::DEFAULT;
    let high = ThumbwheelSensitivity::MAX;
    assert_eq!(zoom_report(&mut wheel, 1, default, now), WheelOutput::Idle);
    assert_eq!(zoom_report(&mut wheel, 4, default, now), WheelOutput::Idle);
    assert_eq!(zoom_report(&mut wheel, 100, high, now), WheelOutput::Idle);
    assert_eq!(zoom_report(&mut wheel, 2, high, now), WheelOutput::Idle);
    assert_eq!(
        zoom_report(&mut wheel, 1, high, now),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_lifecycle_clear_and_mixed_binding_restart_recognition() {
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    let sensitivity = ThumbwheelSensitivity::DEFAULT;
    for interrupted_by_binding in [false, true] {
        assert_eq!(
            zoom_report(&mut wheel, 1, sensitivity, now),
            WheelOutput::Idle
        );
        assert_eq!(
            zoom_report(&mut wheel, 4, sensitivity, now),
            WheelOutput::Idle
        );
        if interrupted_by_binding {
            assert_eq!(
                wheel.advance(
                    WheelRotation::from_increments(-1).unwrap(),
                    &Action::None,
                    unscaled(sensitivity),
                    now,
                ),
                WheelOutput::Idle
            );
        } else {
            wheel.clear_zoom();
        }
        assert_eq!(
            zoom_report(&mut wheel, 100, sensitivity, now),
            WheelOutput::Idle
        );
        wheel.clear_zoom();
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn zoom_configured_actions_share_physical_progress_even_when_bindings_are_reversed() {
    let now = Instant::now();
    let scale = unscaled(ThumbwheelSensitivity::DEFAULT);
    for (up_action, down_action) in [
        (Action::ZoomIn, Action::ZoomOut),
        (Action::ZoomOut, Action::ZoomIn),
    ] {
        let mut wheel = WheelAccumulators::default();
        for (increments, expected) in [
            (1, WheelOutput::Idle),
            (4, WheelOutput::Idle),
            (-4, WheelOutput::Idle),
            (-4, WheelOutput::Idle),
            (-1, WheelOutput::FireAction(1)),
        ] {
            let action = if increments > 0 {
                &up_action
            } else {
                &down_action
            };
            assert_eq!(
                wheel.advance(
                    WheelRotation::from_increments(increments).unwrap(),
                    action,
                    scale,
                    now
                ),
                expected
            );
        }
    }
}

fn phased_zoom_report(
    wheel: &mut WheelAccumulators,
    increments: i16,
    phase: openlogi_hid::thumbwheel::RotationStatus,
    now: Instant,
) -> WheelOutput {
    if wheel.prepare_zoom_report(phase, increments, now) {
        zoom_report(wheel, increments, ThumbwheelSensitivity::DEFAULT, now)
    } else {
        WheelOutput::Idle
    }
}

#[test]
fn zoom_start_then_recognition_are_distinct_from_action_progress() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let now = Instant::now();
    for start_count in [0, 100] {
        let mut wheel = WheelAccumulators::default();
        for (count, phase) in [(start_count, Start), (100, Start), (100, Active)] {
            assert_eq!(
                phased_zoom_report(&mut wheel, count, phase, now),
                WheelOutput::Idle
            );
        }
        assert_eq!(
            phased_zoom_report(&mut wheel, 100, Active, now),
            WheelOutput::FireAction(1)
        );
        assert_eq!(
            phased_zoom_report(&mut wheel, 100, Stop, now),
            WheelOutput::Idle
        );
    }
}

#[test]
fn zoom_missing_start_and_end_phases_are_repaired() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Inactive, Start};
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    // First Active is repaired to Start; the next report recognizes.
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Active, now),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Active, now),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 4, Active, now),
        WheelOutput::Idle
    );
    // A zero Inactive report after Active is repaired to deferred End.
    assert_eq!(
        phased_zoom_report(&mut wheel, 0, Inactive, now),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 1, Start, now + Duration::from_millis(10)),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_deferred_end_preserves_progress_only_before_its_deadline() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let now = Instant::now();
    for (gap, expected) in [(499, WheelOutput::FireAction(1)), (500, WheelOutput::Idle)] {
        let mut wheel = WheelAccumulators::default();
        for (increments, phase) in [(100, Start), (100, Active), (4, Active), (0, Stop)] {
            assert_eq!(
                phased_zoom_report(&mut wheel, increments, phase, now),
                WheelOutput::Idle
            );
        }
        // Duplicate stop does not extend the original deferred end.
        assert_eq!(
            phased_zoom_report(&mut wheel, 100, Stop, now + Duration::from_millis(100)),
            WheelOutput::Idle
        );
        assert_eq!(
            phased_zoom_report(&mut wheel, 1, Start, now + Duration::from_millis(gap)),
            expected
        );
    }
}

#[test]
fn zoom_fresh_start_and_lifecycle_clear_do_not_inherit_progress() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    for (increments, phase) in [
        (100, Start),
        (100, Active),
        (4, Active),
        (1, Start),
        (1, Active),
        (0, Stop),
    ] {
        assert_eq!(
            phased_zoom_report(&mut wheel, increments, phase, now),
            WheelOutput::Idle
        );
    }
    wheel.clear_zoom();
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Start, now),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Active, now),
        WheelOutput::Idle
    );
}

#[test]
fn zoom_repaired_start_keeps_raw_active_for_the_next_comparison() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Inactive, Start};
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    // Repaired Active is still raw Active. A real Start after it is fresh,
    // rather than a duplicate of the repaired Start.
    for phase in [Active, Start, Active] {
        assert_eq!(
            phased_zoom_report(&mut wheel, 100, phase, now),
            WheelOutput::Idle
        );
    }
    assert_eq!(
        phased_zoom_report(&mut wheel, 4, Active, now),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 0, Inactive, now),
        WheelOutput::Idle
    );
    // Raw Inactive followed by Active repairs into a continuing Start.
    assert_eq!(
        phased_zoom_report(&mut wheel, 1, Active, now + Duration::from_millis(1)),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_zero_start_cancels_end_without_recognizing_or_losing_progress() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    for (count, phase) in [(0, Start), (100, Active), (4, Active), (0, Stop)] {
        assert_eq!(
            phased_zoom_report(&mut wheel, count, phase, now),
            WheelOutput::Idle
        );
    }
    let resumed = now + Duration::from_millis(499);
    assert_eq!(
        phased_zoom_report(&mut wheel, 0, Start, resumed),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Start, resumed),
        WheelOutput::Idle
    );
    // The canceled end would have expired here. Exact inactivity is still valid.
    assert_eq!(
        phased_zoom_report(&mut wheel, 1, Active, now + Duration::from_millis(500)),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_deferred_end_retires_progress_on_active_at_the_exact_deadline() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let mut wheel = WheelAccumulators::default();
    let now = Instant::now();
    for (count, phase) in [(0, Start), (100, Active), (4, Active), (100, Stop)] {
        assert_eq!(
            phased_zoom_report(&mut wheel, count, phase, now),
            WheelOutput::Idle
        );
    }
    let deadline = now + Duration::from_millis(500);
    // Unlike ordinary inactivity at exactly 500 ms, deferred End retires now.
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Active, deadline),
        WheelOutput::Idle
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Active, deadline),
        WheelOutput::FireAction(1)
    );
}

#[test]
fn zoom_cancellation_clears_raw_history_and_pending_end() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Start, Stop};
    let now = Instant::now();
    for last_phase in [Start, Stop] {
        let mut wheel = WheelAccumulators::default();
        for phase in [Start, Active, Active, last_phase] {
            phased_zoom_report(&mut wheel, 4, phase, now);
        }
        wheel.clear_zoom();
        let resumed = now + Duration::from_millis(1);
        // First Active repairs to Start after cancellation, regardless of the
        // former raw phase or a pending End.
        for _ in 0..2 {
            assert_eq!(
                phased_zoom_report(&mut wheel, 100, Active, resumed),
                WheelOutput::Idle
            );
        }
        assert_eq!(
            phased_zoom_report(&mut wheel, 4, Active, resumed),
            WheelOutput::Idle
        );
        assert_eq!(
            phased_zoom_report(&mut wheel, 1, Active, now + Duration::from_millis(500)),
            WheelOutput::FireAction(1)
        );
    }
}

#[test]
fn zoom_rotation_without_phase_still_recognizes_and_fires_both_directions() {
    use openlogi_hid::thumbwheel::RotationStatus::Inactive;
    let now = Instant::now();
    for sign in [-1, 1] {
        let mut wheel = WheelAccumulators::default();
        for expected in [
            WheelOutput::Idle,
            WheelOutput::Idle,
            WheelOutput::FireAction(1),
            WheelOutput::FireAction(1),
        ] {
            assert_eq!(
                phased_zoom_report(&mut wheel, 100 * sign, Inactive, now),
                expected
            );
        }
        assert_eq!(
            phased_zoom_report(&mut wheel, 0, Inactive, now),
            WheelOutput::Idle
        );
    }
}

#[test]
fn zoom_unphased_movement_continues_after_a_pending_end() {
    use openlogi_hid::thumbwheel::RotationStatus::{Active, Inactive, Start, Stop};
    let now = Instant::now();
    let mut wheel = WheelAccumulators::default();
    for (count, phase) in [(0, Start), (100, Active), (4, Active), (0, Stop)] {
        assert_eq!(
            phased_zoom_report(&mut wheel, count, phase, now),
            WheelOutput::Idle
        );
    }
    assert_eq!(
        phased_zoom_report(&mut wheel, 1, Inactive, now + Duration::from_millis(10)),
        WheelOutput::FireAction(1)
    );
    assert_eq!(
        phased_zoom_report(&mut wheel, 100, Inactive, now + Duration::from_millis(20)),
        WheelOutput::FireAction(1)
    );
}

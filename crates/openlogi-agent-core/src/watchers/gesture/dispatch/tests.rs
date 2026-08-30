use openlogi_hid::thumbwheel::WheelResolution;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::*;

fn rotation(magnitude: i32) -> WheelRotation {
    let increments = i16::try_from(magnitude).expect("test magnitude fits in i16");
    WheelRotation::from_increments(increments).expect("non-zero test rotation")
}

fn scale() -> ScrollScale {
    ScrollScale::new(WheelResolution::UNKNOWN, ThumbwheelSensitivity::DEFAULT)
}

#[test]
fn replacement_session_does_not_inherit_progress_or_cooldown() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    let now = Instant::now();
    let mut wheels = SessionWheels::default();

    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(threshold), &Action::VolumeUp, scale(), now,),
        WheelOutput::FireAction
    );
    assert_eq!(
        wheels.for_session(&replacement).advance(
            rotation(threshold),
            &Action::VolumeUp,
            scale(),
            now,
        ),
        WheelOutput::FireAction,
        "a new session must not inherit the old session's cooldown"
    );

    wheels.cancel_session(&old);
    assert!(
        wheels.0.contains_key(&replacement),
        "canceling a stale epoch must not erase its replacement's state"
    );
}

#[test]
fn replacement_session_does_not_inherit_partial_progress() {
    let old = HidppSessionId::with_epoch("mouse-a", 7);
    let replacement = HidppSessionId::with_epoch("mouse-a", 8);
    let threshold = ThumbwheelSensitivity::DEFAULT.action_threshold();
    let now = Instant::now();
    let mut wheels = SessionWheels::default();

    assert_eq!(
        wheels
            .for_session(&old)
            .advance(rotation(threshold - 1), &Action::VolumeUp, scale(), now,),
        WheelOutput::Idle
    );
    assert_eq!(
        wheels
            .for_session(&replacement)
            .advance(rotation(1), &Action::VolumeUp, scale(), now,),
        WheelOutput::Idle,
        "a new session must start with no action progress"
    );
}

#[test]
fn touchpad_stroke_freezes_bindings_from_its_first_frame() {
    use openlogi_core::touchpad::TouchContact;

    let frame = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let mut runtime = TouchpadRuntime::default();
    let first_profile = BTreeMap::from([(trigger, Action::Copy)]);
    let replacement_profile = BTreeMap::from([(trigger, Action::Paste)]);

    assert_eq!(
        runtime.update(&frame, &first_profile, true),
        TouchpadOutput::Idle
    );
    // A foreground-app change can replace the live plan before lift. The tap
    // must still resolve against the profile active when the stroke began.
    assert_eq!(
        runtime.end(true),
        TouchpadOutput::Action {
            trigger: ButtonId::TouchpadTwoFingerTap,
            action: Action::Copy
        }
    );

    assert_eq!(
        runtime.update(&frame, &replacement_profile, true),
        TouchpadOutput::Idle
    );
    assert_eq!(
        runtime.end(true),
        TouchpadOutput::Action {
            trigger: ButtonId::TouchpadTwoFingerTap,
            action: Action::Paste
        }
    );
}

#[test]
fn diagnostic_touchpad_stroke_cannot_fire_if_management_enables_mid_stroke() {
    use openlogi_core::touchpad::TouchContact;

    let frame = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let bindings = BTreeMap::from([(trigger, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    assert_eq!(
        runtime.update(&frame, &bindings, false),
        TouchpadOutput::Idle
    );
    assert_eq!(runtime.end(true), TouchpadOutput::Idle);

    assert_eq!(
        runtime.update(&frame, &bindings, true),
        TouchpadOutput::Idle
    );
    assert_eq!(
        runtime.end(true),
        TouchpadOutput::Action {
            trigger,
            action: Action::Copy
        }
    );
}

#[test]
fn touchpad_scroll_streams_phases_and_terminates_on_end() {
    use openlogi_core::touchpad::TouchContact;

    let resting = |travelled: u32| {
        TouchFrame::new(
            0,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: 40_000 + travelled,
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: 60_000 + travelled,
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    assert_eq!(
        runtime.update(&resting(0), &bindings, true),
        TouchpadOutput::Idle
    );
    // Under the activation travel: no stream opens yet.
    assert_eq!(
        runtime.update(&resting(2_000), &bindings, true),
        TouchpadOutput::Idle
    );
    assert_eq!(
        runtime.update(&resting(5_000), &bindings, true),
        TouchpadOutput::Scroll {
            dx_um: 3_000,
            dy_um: 0,
            phase: SmoothScrollPhase::Began
        }
    );
    assert_eq!(
        runtime.update(&resting(7_000), &bindings, true),
        TouchpadOutput::Scroll {
            dx_um: 2_000,
            dy_um: 0,
            phase: SmoothScrollPhase::Changed
        }
    );
    // The stroke ends without a tap: only the scroll terminator routes,
    // carrying the exit velocity for the momentum gate.
    assert_eq!(
        runtime.end(true),
        TouchpadOutput::ScrollEnd {
            phase: SmoothScrollPhase::Ended,
            exit_velocity_um_per_s: Some((0.0, 0.0)),
        }
    );
    assert_eq!(runtime.end(true), TouchpadOutput::Idle);
}

#[test]
fn touchpad_scroll_survives_disabled_actions_and_cancels_cleanly() {
    use openlogi_core::touchpad::TouchContact;

    let resting = |travelled: u32| {
        TouchFrame::new(
            0,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: 40_000 + travelled,
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: 60_000 + travelled,
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    runtime.update(&resting(0), &bindings, true);
    runtime.update(&resting(2_000), &bindings, false);
    // Actions off must not stop the scroll: it replaces the firmware
    // scrolling the capture itself disabled, not a bound gesture.
    assert_eq!(
        runtime.update(&resting(5_000), &bindings, false),
        TouchpadOutput::Scroll {
            dx_um: 3_000,
            dy_um: 0,
            phase: SmoothScrollPhase::Began
        }
    );
    assert_eq!(
        runtime.cancel(),
        TouchpadOutput::ScrollEnd {
            phase: SmoothScrollPhase::Cancelled,
            exit_velocity_um_per_s: None,
        }
    );
    assert_eq!(runtime.cancel(), TouchpadOutput::Idle);
}

#[test]
fn touchpad_scroll_tuning_scales_and_inverts_content_deltas() {
    use openlogi_core::config::TouchpadScrollSensitivity;

    fn tuning(sensitivity: TouchpadScrollSensitivity, inverted: bool) -> TouchpadScrollTuning {
        TouchpadScrollTuning::from_plan(&DispatchPlan {
            config_key: "casa".to_string(),
            bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            side_gesture_bindings: BTreeMap::new(),
            thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
            touchpad_bindings: BTreeMap::new(),
            touchpad_scroll_sensitivity: sensitivity,
            touchpad_scroll_inverted: inverted,
        })
    }
    fn assert_pixels(delta: openlogi_core::scroll::ScrollDelta, x: f64, y: f64) {
        assert!((delta.x() - x).abs() < 1e-9, "x: {}", delta.x());
        assert!((delta.y() - y).abs() < 1e-9, "y: {}", delta.y());
    }

    // Neutral tuning keeps the base 25 px/mm gain with the content-following
    // axis mapping (horizontal negated, vertical as-is).
    assert_pixels(
        tuning(TouchpadScrollSensitivity::DEFAULT, false).content_delta(1_000, 2_000),
        -25.0,
        50.0,
    );
    // Doubling the sensitivity doubles both axes.
    let doubled = TouchpadScrollSensitivity::try_new(28).expect("valid sensitivity");
    assert_pixels(
        tuning(doubled, false).content_delta(1_000, 2_000),
        -50.0,
        100.0,
    );
    // Inversion flips both axes on top of the gain.
    assert_pixels(
        tuning(TouchpadScrollSensitivity::DEFAULT, true).content_delta(1_000, 2_000),
        25.0,
        -50.0,
    );
}

#[test]
fn touchpad_scroll_exit_velocity_tracks_frames_and_releases_slowly() {
    use openlogi_core::touchpad::TouchContact;

    let travelling = |timestamp_us: u64, travelled: u32| {
        TouchFrame::new(
            timestamp_us,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: 40_000 + travelled,
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: 60_000 + travelled,
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    // Steady 3 mm per 25 ms frame = 120 mm/s to the right.
    runtime.update(&travelling(0, 0), &bindings, true);
    runtime.update(&travelling(25_000, 2_000), &bindings, true);
    runtime.update(&travelling(50_000, 5_000), &bindings, true);
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _vy)),
        ..
    } = runtime.end(true)
    else {
        panic!("a streamed stroke must report its exit velocity");
    };
    assert!((vx - 120_000.0).abs() < 1.0, "got {vx} µm/s");

    // One slow frame right before lift does not kill the glide: the filter
    // releases at α = 0.01, so the smoothed speed stays near the fast phase.
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&travelling(0, 0), &bindings, true);
    runtime.update(&travelling(25_000, 2_000), &bindings, true);
    runtime.update(&travelling(50_000, 5_000), &bindings, true);
    runtime.update(&travelling(75_000, 5_500), &bindings, true);
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _)),
        ..
    } = runtime.end(true)
    else {
        panic!("streamed stroke");
    };
    assert!(
        vx > 100_000.0,
        "a single slow frame must not collapse the exit velocity, got {vx}"
    );
}

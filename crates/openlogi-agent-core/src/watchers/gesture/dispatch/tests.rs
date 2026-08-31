use openlogi_core::touchpad::{TouchContact, TouchFrame};
use openlogi_hid::thumbwheel::WheelResolution;
use openlogi_inject::DockSwipeMotion;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::*;

fn contact(id: u8, x_um: u32, y_um: u32) -> TouchContact {
    TouchContact { id, x_um, y_um }
}

fn frame(timestamp_us: u64, contacts: Vec<TouchContact>) -> TouchFrame {
    TouchFrame::new(timestamp_us, false, contacts).expect("test contacts have unique ids")
}

fn translated_frame(
    timestamp_us: u64,
    count: u8,
    horizontal_um: i32,
    vertical_um: i32,
) -> TouchFrame {
    let contacts = (0..count)
        .map(|id| {
            let x = 50_000_i32 + i32::from(id) * 10_000 + horizontal_um;
            let y = 50_000_i32 + vertical_um;
            contact(
                id + 1,
                u32::try_from(x).expect("test x stays positive"),
                u32::try_from(y).expect("test y stays positive"),
            )
        })
        .collect();
    frame(timestamp_us, contacts)
}

fn idle() -> TouchpadOutcome {
    TouchpadOutcome::default()
}

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

    let contacts = vec![
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
    ];
    let frame = TouchFrame::new(1_000, false, contacts.clone()).expect("valid frame");
    // One aged frame: a tap only resolves once it held for 30 ms.
    let aged = TouchFrame::new(50_000, false, contacts).expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let mut runtime = TouchpadRuntime::default();
    let first_profile = BTreeMap::from([(trigger, Action::Copy)]);
    let replacement_profile = BTreeMap::from([(trigger, Action::Paste)]);

    assert_eq!(runtime.update(&frame, &first_profile, true, false), idle());
    assert_eq!(
        runtime.update(&aged, &replacement_profile, true, false),
        idle()
    );
    // A foreground-app change can replace the live plan before lift. The tap
    // must still resolve against the profile active when the stroke began.
    assert_eq!(
        runtime.end(true).routed,
        TouchpadOutput::Action {
            trigger: ButtonId::TouchpadTwoFingerTap,
            action: Action::Copy
        }
    );

    assert_eq!(
        runtime.update(&frame, &replacement_profile, true, false),
        idle()
    );
    assert_eq!(runtime.update(&aged, &first_profile, true, false), idle());
    assert_eq!(
        runtime.end(true).routed,
        TouchpadOutput::Action {
            trigger: ButtonId::TouchpadTwoFingerTap,
            action: Action::Paste
        }
    );
}

#[test]
fn a_touch_within_the_glide_suppression_window_cannot_tap() {
    use openlogi_core::touchpad::TouchContact;

    let contacts = vec![
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
    ];
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    runtime.suppress_taps(std::time::Duration::from_millis(500));
    assert_eq!(
        runtime.update(
            &TouchFrame::new(1_000, false, contacts.clone()).expect("valid frame"),
            &bindings,
            true,
            false
        ),
        idle()
    );
    assert_eq!(
        runtime.update(
            &TouchFrame::new(50_000, false, contacts).expect("valid frame"),
            &bindings,
            true,
            false
        ),
        idle()
    );
    // The stopping touch of a glide must not resolve into its bound action.
    assert_eq!(runtime.end(true).routed, TouchpadOutput::Idle);
}

#[test]
fn diagnostic_touchpad_stroke_cannot_fire_if_management_enables_mid_stroke() {
    use openlogi_core::touchpad::TouchContact;

    let contacts = vec![
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
    ];
    let frame = TouchFrame::new(1_000, false, contacts.clone()).expect("valid frame");
    // One aged frame: a tap only resolves once it held for 30 ms.
    let aged = TouchFrame::new(50_000, false, contacts).expect("valid frame");
    let trigger = ButtonId::TouchpadTwoFingerTap;
    let bindings = BTreeMap::from([(trigger, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    assert_eq!(runtime.update(&frame, &bindings, false, false), idle());
    assert_eq!(runtime.end(true).routed, TouchpadOutput::Idle);

    assert_eq!(runtime.update(&frame, &bindings, true, false), idle());
    assert_eq!(runtime.update(&aged, &bindings, true, false), idle());
    assert_eq!(
        runtime.end(true).routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::Copy
        }
    );
}

#[test]
fn native_swipe_streams_progress_instead_of_dispatching() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();

    assert_eq!(
        runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true),
        idle()
    );
    assert_eq!(
        runtime.update(
            &translated_frame(60_000, 3, 15_000, 0),
            &bindings,
            true,
            true
        ),
        idle()
    );
    let outcome = runtime.update(
        &translated_frame(90_000, 3, 25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: 10_000.0 / 117_000.0,
        }
    );
    let outcome = runtime.update(
        &translated_frame(120_000, 3, 30_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Advance {
            motion: DockSwipeMotion::Horizontal,
            delta: 5_000.0 / 117_000.0,
        }
    );

    let outcome = runtime.end(true);
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Finish {
            motion: DockSwipeMotion::Horizontal,
            end: SwipeEnd::AtRelease,
        }
    );
}

#[test]
fn touchpad_scroll_streams_deltas_and_terminates_on_end() {
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
        runtime.update(&resting(0), &bindings, true, false).routed,
        TouchpadOutput::Idle
    );
    // Under the activation travel: no stream opens yet.
    assert_eq!(
        runtime
            .update(&resting(2_000), &bindings, true, false)
            .routed,
        TouchpadOutput::Idle
    );
    assert_eq!(
        runtime
            .update(&resting(5_000), &bindings, true, false)
            .routed,
        TouchpadOutput::Scroll {
            dx_um: 3_000,
            dy_um: 0
        }
    );
    assert_eq!(
        runtime
            .update(&resting(7_000), &bindings, true, false)
            .routed,
        TouchpadOutput::Scroll {
            dx_um: 2_000,
            dy_um: 0
        }
    );
    // The stroke ends without a tap: only the scroll terminator routes,
    // carrying the exit velocity for the momentum gate.
    assert_eq!(
        runtime.end(true).routed,
        TouchpadOutput::ScrollEnd {
            exit_velocity_um_per_s: Some((0.0, 0.0)),
        }
    );
    assert_eq!(runtime.end(true).routed, TouchpadOutput::Idle);
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

    runtime.update(&resting(0), &bindings, true, false);
    runtime.update(&resting(2_000), &bindings, false, false);
    // Actions off must not stop the scroll: it replaces the firmware
    // scrolling the capture itself disabled, not a bound gesture.
    assert_eq!(
        runtime
            .update(&resting(5_000), &bindings, false, false)
            .routed,
        TouchpadOutput::Scroll {
            dx_um: 3_000,
            dy_um: 0,
        }
    );
    assert_eq!(
        runtime.cancel().routed,
        TouchpadOutput::ScrollEnd {
            exit_velocity_um_per_s: None,
        }
    );
    assert_eq!(runtime.cancel().routed, TouchpadOutput::Idle);
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
    runtime.update(&travelling(0, 0), &bindings, true, false);
    runtime.update(&travelling(25_000, 2_000), &bindings, true, false);
    runtime.update(&travelling(50_000, 5_000), &bindings, true, false);
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _vy)),
        ..
    } = runtime.end(true).routed
    else {
        panic!("a streamed stroke must report its exit velocity");
    };
    assert!((vx - 120_000.0).abs() < 1.0, "got {vx} µm/s");

    // One slow frame right before lift does not kill the glide: the filter
    // releases at α = 0.01, so the smoothed speed stays near the fast phase.
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&travelling(0, 0), &bindings, true, false);
    runtime.update(&travelling(25_000, 2_000), &bindings, true, false);
    runtime.update(&travelling(50_000, 5_000), &bindings, true, false);
    runtime.update(&travelling(75_000, 5_500), &bindings, true, false);
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _)),
        ..
    } = runtime.end(true).routed
    else {
        panic!("streamed stroke");
    };
    assert!(
        vx > 100_000.0,
        "a single slow frame must not collapse the exit velocity, got {vx}"
    );
}

#[test]
fn a_same_axis_reversal_re_aims_the_exit_velocity() {
    use openlogi_core::touchpad::TouchContact;

    let travelling = |timestamp_us: u64, travelled: i32| {
        TouchFrame::new(
            timestamp_us,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: u32::try_from(50_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: u32::try_from(60_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    // Right at 120 mm/s, then an immediate leftward flick at 80 mm/s: the
    // glide must follow the new direction, not average against the stale one.
    runtime.update(&travelling(0, 0), &bindings, true, false);
    runtime.update(&travelling(25_000, 3_000), &bindings, true, false);
    runtime.update(&travelling(50_000, 6_000), &bindings, true, false);
    runtime.update(&travelling(75_000, 4_000), &bindings, true, false);
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _)),
    } = runtime.end(true).routed
    else {
        panic!("streamed stroke");
    };
    assert!(
        vx < -60_000.0,
        "the reversal must re-aim the exit velocity, got {vx} µm/s"
    );
}

#[test]
fn left_swipes_stream_negative_progress() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeLeft;
    let bindings = BTreeMap::from([(trigger, Action::PreviousDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, -15_000, 0),
        &bindings,
        true,
        true,
    );

    let outcome = runtime.update(
        &translated_frame(90_000, 3, -25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: -10_000.0 / 117_000.0,
        }
    );
}

#[test]
fn a_deliberate_hold_before_lift_kills_the_glide() {
    use openlogi_core::touchpad::TouchContact;

    let travelling = |timestamp_us: u64, travelled: i32| {
        TouchFrame::new(
            timestamp_us,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: u32::try_from(50_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: u32::try_from(60_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    // A fast scroll, then the fingers sit still for a full second before
    // lifting: with the dt-normalized release (τ = 200 ms) the memory of the
    // fast phase has fully decayed — no glide out of a deliberate stop.
    runtime.update(&travelling(0, 0), &bindings, true, false);
    runtime.update(&travelling(25_000, 3_000), &bindings, true, false);
    runtime.update(&travelling(50_000, 6_000), &bindings, true, false);
    for step in 1..=50 {
        runtime.update(
            &travelling(50_000 + step * 20_000, 6_000),
            &bindings,
            true,
            false,
        );
    }
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, _)),
    } = runtime.end(true).routed
    else {
        panic!("streamed stroke");
    };
    assert!(
        vx.abs() < 5_000.0,
        "a held stop must not glide, got {vx} µm/s"
    );
}

#[test]
fn unsupported_platform_keeps_discrete_swipe_dispatch() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, false);

    let outcome = runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        false,
    );
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::NextDesktop
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn vertical_up_swipes_stream_positive_progress() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeUp;
    let bindings = BTreeMap::from([(trigger, Action::MissionControl)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 0, -15_000),
        &bindings,
        true,
        true,
    );

    let outcome = runtime.update(
        &translated_frame(90_000, 3, 0, -25_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Vertical,
            progress: 10_000.0 / 75_600.0,
        }
    );
    let outcome = runtime.update(
        &translated_frame(120_000, 3, 0, -30_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Advance {
            motion: DockSwipeMotion::Vertical,
            delta: 5_000.0 / 75_600.0,
        }
    );
}

#[test]
fn vertical_down_swipes_stream_negative_progress() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeDown;
    let bindings = BTreeMap::from([(trigger, Action::AppExpose)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 0, 15_000),
        &bindings,
        true,
        true,
    );

    let outcome = runtime.update(
        &translated_frame(90_000, 3, 0, 25_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Vertical,
            progress: -10_000.0 / 75_600.0,
        }
    );
}

#[test]
fn cross_axis_pair_streams_the_vertical_motion() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::MissionControl)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        true,
    );

    let outcome = runtime.update(
        &translated_frame(90_000, 3, 25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Vertical,
            progress: 10_000.0 / 117_000.0,
        }
    );
}

#[test]
fn dropped_frame_cancel_keeps_the_stream_running() {
    let trigger = ButtonId::TouchpadFourFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 4, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 4, 15_000, 0),
        &bindings,
        true,
        true,
    );

    runtime.cancel();
    let outcome = runtime.update(
        &translated_frame(90_000, 4, 25_000, 0),
        &bindings,
        true,
        true,
    );
    assert!(matches!(outcome.stream, SwipeOutput::Begin { .. }));

    let outcome = runtime.end(true);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Finish {
            motion: DockSwipeMotion::Horizontal,
            end: SwipeEnd::AtRelease,
        }
    );
}

#[test]
fn contact_set_change_does_not_jump_progress() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        true,
    );

    // Dropping contact 3 rebases the centroid without emitting progress.
    let rebased = frame(
        90_000,
        vec![contact(1, 65_000, 50_000), contact(2, 75_000, 50_000)],
    );
    let outcome = runtime.update(&rebased, &bindings, true, true);
    assert_eq!(outcome.stream, SwipeOutput::Idle);

    let moved = frame(
        120_000,
        vec![contact(1, 70_000, 50_000), contact(2, 80_000, 50_000)],
    );
    let outcome = runtime.update(&moved, &bindings, true, true);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: 5_000.0 / 117_000.0,
        }
    );
}

#[test]
fn session_teardown_cancels_the_running_animation() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeLeft;
    let bindings = BTreeMap::from([(trigger, Action::PreviousDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, -15_000, 0),
        &bindings,
        true,
        true,
    );
    runtime.update(
        &translated_frame(90_000, 3, -25_000, 0),
        &bindings,
        true,
        true,
    );

    assert_eq!(
        runtime.terminate().stream,
        SwipeOutput::Finish {
            motion: DockSwipeMotion::Horizontal,
            end: SwipeEnd::Cancelled,
        }
    );
}

#[test]
fn an_unopened_stream_fires_its_committed_action_at_release() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        true,
    );

    // The stroke committed on its very last frame of travel: no animation
    // ever opened, so the suppressed discrete dispatch must fire at release —
    // an ultra-short swipe must not lose its binding.
    let outcome = runtime.end(true);
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::NextDesktop
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn four_finger_swipes_stream_like_three_finger_ones() {
    let trigger = ButtonId::TouchpadFourFingerSwipeLeft;
    let bindings = BTreeMap::from([(trigger, Action::PreviousDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 4, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 4, -15_000, 0),
        &bindings,
        true,
        true,
    );
    let outcome = runtime.update(
        &translated_frame(90_000, 4, -25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: -10_000.0 / 117_000.0,
        }
    );
}

#[test]
fn begin_failure_falls_back_to_discrete_action() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeRight;
    let bindings = BTreeMap::from([(trigger, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        true,
    );
    let outcome = runtime.update(
        &translated_frame(90_000, 3, 25_000, 0),
        &bindings,
        true,
        true,
    );
    let SwipeOutput::Begin { progress, .. } = outcome.stream else {
        panic!("the swipe must have opened its stream");
    };

    // The fallback is the action whose animation failed to begin — the slot
    // of the progress sign the stream was opening toward.
    assert_eq!(
        runtime.begin_failed(progress),
        Some((trigger, Action::NextDesktop))
    );

    let outcome = runtime.update(
        &translated_frame(120_000, 3, 30_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
    let outcome = runtime.end(true);
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn unbound_side_clamps_progress_at_zero() {
    let bindings = BTreeMap::from([(ButtonId::TouchpadThreeFingerSwipeUp, Action::MissionControl)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);

    // The down direction is unbound, but its commit still opens the pair's
    // stream: downward travel pins progress at zero instead of dying.
    let outcome = runtime.update(
        &translated_frame(60_000, 3, 0, 15_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome, idle());
    let outcome = runtime.update(
        &translated_frame(90_000, 3, 0, 25_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome, idle());

    // Reversing upward tracks the finger one-to-one from the first frame:
    // the pinned downward travel was never accumulated, so it does not have
    // to be eaten through before the animation follows.
    let outcome = runtime.update(
        &translated_frame(120_000, 3, 0, 5_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Vertical,
            progress: 20_000.0 / 75_600.0,
        }
    );

    // Dragging back follows the animation down to zero, then pins there —
    // the unbound side can pull back, never commit.
    let outcome = runtime.update(
        &translated_frame(150_000, 3, 0, 30_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Advance {
            motion: DockSwipeMotion::Vertical,
            delta: -20_000.0 / 75_600.0,
        }
    );
    let outcome = runtime.update(
        &translated_frame(180_000, 3, 0, 40_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);

    // The opened stream still releases; progress sits at zero, so the
    // injector's sign rule springs it back instead of committing.
    let outcome = runtime.end(true);
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Finish {
            motion: DockSwipeMotion::Vertical,
            end: SwipeEnd::AtRelease,
        }
    );
}

#[test]
fn a_reversed_binding_flips_the_travel_mapping() {
    let bindings = BTreeMap::from([(ButtonId::TouchpadThreeFingerSwipeLeft, Action::NextDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, -15_000, 0),
        &bindings,
        true,
        true,
    );

    // Leftward fingers bound to the rightward-commit consumer: the mapping
    // flips so the animation commits the bound action, not the native one.
    let outcome = runtime.update(
        &translated_frame(90_000, 3, -25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(outcome.routed, TouchpadOutput::Idle);
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: 10_000.0 / 117_000.0,
        }
    );
}

#[test]
fn a_fully_bound_pair_streams_both_directions() {
    let bindings = BTreeMap::from([
        (
            ButtonId::TouchpadThreeFingerSwipeLeft,
            Action::PreviousDesktop,
        ),
        (ButtonId::TouchpadThreeFingerSwipeRight, Action::NextDesktop),
    ]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);
    runtime.update(
        &translated_frame(60_000, 3, 15_000, 0),
        &bindings,
        true,
        true,
    );

    let outcome = runtime.update(
        &translated_frame(90_000, 3, 25_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Begin {
            motion: DockSwipeMotion::Horizontal,
            progress: 10_000.0 / 117_000.0,
        }
    );

    // Travel back past the anchor crosses into the other bound direction's
    // progress instead of clamping. Progress runs +10k → −20k, so the delta
    // spans the whole 30k frame.
    let outcome = runtime.update(
        &translated_frame(120_000, 3, -5_000, 0),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.stream,
        SwipeOutput::Advance {
            motion: DockSwipeMotion::Horizontal,
            delta: -30_000.0 / 117_000.0,
        }
    );
}

#[test]
fn mixed_motion_bindings_keep_the_pair_discrete() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeUp;
    let bindings = BTreeMap::from([
        (trigger, Action::MissionControl),
        (ButtonId::TouchpadThreeFingerSwipeDown, Action::NextDesktop),
    ]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);

    let outcome = runtime.update(
        &translated_frame(60_000, 3, 0, -15_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::MissionControl
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn the_same_action_on_both_sides_keeps_the_pair_discrete() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeUp;
    let bindings = BTreeMap::from([
        (trigger, Action::MissionControl),
        (
            ButtonId::TouchpadThreeFingerSwipeDown,
            Action::MissionControl,
        ),
    ]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);

    let outcome = runtime.update(
        &translated_frame(60_000, 3, 0, -15_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::MissionControl
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn show_desktop_bindings_keep_the_pair_discrete() {
    let trigger = ButtonId::TouchpadThreeFingerSwipeUp;
    let bindings = BTreeMap::from([(trigger, Action::ShowDesktop)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(&translated_frame(0, 3, 0, 0), &bindings, true, true);

    let outcome = runtime.update(
        &translated_frame(60_000, 3, 0, -15_000),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::ShowDesktop
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn pinch_triggers_never_stream() {
    let trigger = ButtonId::TouchpadTwoFingerPinchOut;
    let bindings = BTreeMap::from([(trigger, Action::ZoomIn)]);
    let mut runtime = TouchpadRuntime::default();
    runtime.update(
        &frame(
            0,
            vec![contact(1, 40_000, 50_000), contact(2, 60_000, 50_000)],
        ),
        &bindings,
        true,
        true,
    );

    // Spreading the pair outward past the pinch threshold commits PinchOut,
    // which has no native swipe animation to stream — discrete it stays.
    let outcome = runtime.update(
        &frame(
            60_000,
            vec![contact(1, 30_000, 50_000), contact(2, 70_000, 50_000)],
        ),
        &bindings,
        true,
        true,
    );
    assert_eq!(
        outcome.routed,
        TouchpadOutput::Action {
            trigger,
            action: Action::ZoomIn
        }
    );
    assert_eq!(outcome.stream, SwipeOutput::Idle);
}

#[test]
fn a_button_press_at_any_contact_count_kills_the_glide() {
    use openlogi_core::touchpad::TouchContact;

    let travelling = |timestamp_us: u64, travelled: i32| {
        TouchFrame::new(
            timestamp_us,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: u32::try_from(50_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
                TouchContact {
                    id: 2,
                    x_um: u32::try_from(60_000_i32 + travelled).expect("positive"),
                    y_um: 50_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let button_held = |timestamp_us: u64| {
        TouchFrame::new(
            timestamp_us,
            true,
            vec![TouchContact {
                id: 1,
                x_um: 56_000,
                y_um: 50_000,
            }],
        )
        .expect("valid frame")
    };
    let bindings = BTreeMap::from([(ButtonId::TouchpadTwoFingerTap, Action::Copy)]);
    let mut runtime = TouchpadRuntime::default();

    // A fast scroll, then the button lands with a single finger still down:
    // the pressed button owns the pad, so the scroll it cut into must not
    // glide on lift — the exit velocity dies at the button frame itself.
    runtime.update(&travelling(0, 0), &bindings, true, false);
    runtime.update(&travelling(25_000, 3_000), &bindings, true, false);
    runtime.update(&travelling(50_000, 6_000), &bindings, true, false);
    assert_eq!(
        runtime.update(&button_held(75_000), &bindings, true, false),
        idle()
    );
    let TouchpadOutput::ScrollEnd {
        exit_velocity_um_per_s: Some((vx, vy)),
    } = runtime.end(true).routed
    else {
        panic!("streamed stroke");
    };
    assert_eq!((vx, vy), (0.0, 0.0));
}

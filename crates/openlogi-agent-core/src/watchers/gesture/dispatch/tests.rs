use std::collections::BTreeMap;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::config::{ThumbwheelSensitivity, ZoomSensitivity};
use openlogi_core::hid::Dpi;
use openlogi_hid::thumbwheel::WheelResolution;

use crate::capture_plan::DispatchPlan;
use crate::runtime::HidppSessionId;

use super::wheel::{ScrollScale, WheelOutput, WheelRotation};
use super::{SessionWheels, hidpp_click_binding, hidpp_hold_click_suppressed};

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

fn click_plan(hold: Option<(ButtonId, Action)>, click: &(ButtonId, Action)) -> DispatchPlan {
    let mut bindings = BTreeMap::from([(click.0, Binding::Single(click.1.clone()))]);
    let mut hold_bindings = BTreeMap::new();
    if let Some((button, action)) = hold {
        bindings.insert(button, Binding::Single(action.clone()));
        hold_bindings.insert(button, action);
    }
    DispatchPlan {
        config_key: "mouse-a".into(),
        bindings,
        gesture_bindings: BTreeMap::new(),
        side_gesture_bindings: BTreeMap::new(),
        thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
        hold_bindings,
        sensor_dpi: Some(Dpi::new(1000)),
        zoom_sensitivity: ZoomSensitivity::DEFAULT,
        invert_pan: false,
    }
}

#[test]
fn unarmed_hold_binding_is_not_a_click_or_a_scroll() {
    // Against the previous resolver this returned Some(Pan) and the click
    // path would hand Pan to execute() — AgentSide today, a silent scroll
    // the day someone wires it. The hold must do nothing as a click.
    let plan = click_plan(
        Some((ButtonId::Forward, Action::Pan)),
        &(ButtonId::MiddleClick, Action::Copy),
    );
    assert_eq!(
        hidpp_click_binding(&plan, ButtonId::Forward),
        None,
        "Forward = Pan must not fire as a one-shot action"
    );
    assert_eq!(
        hidpp_click_binding(&plan, ButtonId::Back),
        None,
        "an unbound hold-adjacent button must not invent a binding"
    );
    assert_eq!(
        hidpp_click_binding(&plan, ButtonId::MiddleClick).map(Binding::click_action),
        Some(Action::Copy),
        "stripping hold-mode must not drop a real click binding"
    );
    assert_eq!(
        hidpp_hold_click_suppressed(&plan, ButtonId::Forward),
        Some(&Action::Pan)
    );
}

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::config::{ThumbwheelSensitivity, ZoomSensitivity};
use openlogi_core::hid::Dpi;

use openlogi_hid::HoldRelease;

use super::{HoldCommand, HoldSessions, MM_PER_INCH, millimetres, pan_pixels, zoom_magnification};
use crate::capture_plan::DispatchPlan;
use crate::runtime::HidppSessionId;

/// The user let go after clearing the click/drag deadzone.
const DRAG: HoldRelease = HoldRelease::Released { traveled: true };
/// The user let go without clearing it.
const CLICK: HoldRelease = HoldRelease::Released { traveled: false };

fn session(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("mouse-a", epoch)
}

fn plan(button: ButtonId, action: Action, dpi: u16) -> DispatchPlan {
    DispatchPlan {
        config_key: "mouse-a".into(),
        bindings: BTreeMap::from([(button, Binding::Single(action.clone()))]),
        gesture_bindings: BTreeMap::new(),
        side_gesture_bindings: BTreeMap::new(),
        thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
        hold_bindings: BTreeMap::from([(button, action)]),
        sensor_dpi: Some(Dpi::new(dpi)),
        zoom_sensitivity: ZoomSensitivity::DEFAULT,
        invert_pan: false,
    }
}

fn pan_plan(dpi: u16) -> DispatchPlan {
    plan(ButtonId::Back, Action::Pan, dpi)
}

fn zoom_plan(dpi: u16) -> DispatchPlan {
    plan(ButtonId::Forward, Action::Zoom, dpi)
}

/// Counts for `mm` millimetres of travel at `dpi`. This is the DPI
/// definition (`counts / dpi` inches × 25.4), not a copy of the pan/zoom
/// scale constants.
fn counts_for_mm(mm: f32, dpi: u16) -> i16 {
    let rounded = (mm * f32::from(dpi) / MM_PER_INCH).round();
    assert!(
        (f32::from(i16::MIN)..=f32::from(i16::MAX)).contains(&rounded),
        "test travel stays inside i16"
    );
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rounded value is range-checked against i16 above"
    )]
    {
        rounded as i16
    }
}

#[test]
fn one_inch_of_travel_is_dpi_independent() {
    // 25.4 mm is one inch. At 1000 DPI that is 1000 counts by definition.
    assert_eq!(counts_for_mm(25.4, 1000), 1000);
    let inch_at_1000 = millimetres(1000, Dpi::new(1000));
    let inch_at_1600 = millimetres(counts_for_mm(25.4, 1600), Dpi::new(1600));
    assert!(
        (inch_at_1000 - 25.4).abs() < 0.05,
        "1000 counts at 1000 DPI must be one inch, got {inch_at_1000} mm"
    );
    assert!(
        (inch_at_1600 - 25.4).abs() < 0.05,
        "the same inch at 1600 DPI must not scale with the extra counts"
    );
}

#[test]
fn one_1080p_screen_of_pan_is_about_two_inches() {
    // 1080 px / 22 px per mm ≈ 49.1 mm. At 1000 DPI that is not 1080 counts
    // (a counts-as-pixels mapping) and not 12 counts (the old deadzone).
    let (dx, dy) = pan_pixels(counts_for_mm(49.1, 1000), 0, Dpi::new(1000));
    assert!(
        (dx - 1080.0).abs() < 15.0,
        "49.1 mm of travel should pan one 1080p screen, got {dx} px"
    );
    assert!(
        dy.abs() < f32::EPSILON,
        "horizontal travel must not invent a vertical pan, got {dy}"
    );

    let (low, _) = pan_pixels(counts_for_mm(49.1, 800), 0, Dpi::new(800));
    let (high, _) = pan_pixels(counts_for_mm(49.1, 1600), 0, Dpi::new(1600));
    assert!(
        (low - high).abs() < 15.0,
        "the same millimetres must pan the same pixels at 800 and 1600 DPI"
    );
}

#[test]
fn twenty_millimetres_of_upward_drag_doubles_zoom() {
    // Dragging up is negative raw-XY. 20 mm × 0.05 / mm = 1.0.
    let default = ZoomSensitivity::DEFAULT;
    let at_1000 = zoom_magnification(counts_for_mm(-20.0, 1000), Dpi::new(1000), default);
    let at_2000 = zoom_magnification(counts_for_mm(-20.0, 2000), Dpi::new(2000), default);
    assert!(
        (at_1000 - 1.0).abs() < 0.03,
        "20 mm up at 1000 DPI should double the view, got {at_1000}"
    );
    assert!(
        (at_2000 - 1.0).abs() < 0.03,
        "the same 20 mm at 2000 DPI must not double twice"
    );
}

#[test]
fn pan_begin_stream_end() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let plan = pan_plan(1000);

    assert_eq!(
        holds.begin(&session, ButtonId::Back, &plan),
        Some(HoldCommand::PanBegin)
    );
    let inch = counts_for_mm(25.4, 1000);
    let (dx, dy) = pan_pixels(inch, 0, Dpi::new(1000));
    assert_eq!(
        holds.motion(&session, ButtonId::Back, inch, 0),
        Some(HoldCommand::Pan { dx, dy })
    );
    assert_eq!(
        holds.end(&session, ButtonId::Back, DRAG),
        Some(HoldCommand::PanEnd)
    );
}

#[test]
fn end_without_motion_still_closes_pan() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    assert_eq!(
        holds.begin(&session, ButtonId::Back, &pan_plan(1000)),
        Some(HoldCommand::PanBegin)
    );
    assert_eq!(
        holds.end(&session, ButtonId::Back, CLICK),
        Some(HoldCommand::PanEnd)
    );
}

#[test]
fn zoom_opens_on_first_motion_and_ends_on_button_up() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let plan = zoom_plan(1000);

    assert_eq!(holds.begin(&session, ButtonId::Forward, &plan), None);
    let amount = zoom_magnification(-200, Dpi::new(1000), ZoomSensitivity::DEFAULT);
    assert_eq!(
        holds.motion(&session, ButtonId::Forward, 0, -200),
        Some(HoldCommand::Zoom { amount })
    );
    assert_eq!(
        holds.end(&session, ButtonId::Forward, DRAG),
        Some(HoldCommand::ZoomEnd)
    );
}

#[test]
fn teardown_then_late_motion_does_not_reopen() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    assert_eq!(
        holds.begin(&session, ButtonId::Forward, &zoom_plan(1000)),
        None
    );
    assert_eq!(holds.close_session(&session), Some(HoldCommand::ZoomEnd));
    assert_eq!(
        holds.motion(&session, ButtonId::Forward, 0, -400),
        None,
        "late motion must not call post_zoom_continuous after teardown"
    );
    assert_eq!(
        holds.begin(&session, ButtonId::Forward, &zoom_plan(1000)),
        None,
        "a closed epoch must not accept a new begin"
    );
}

#[test]
fn teardown_then_late_end_does_not_emit() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    holds.begin(&session, ButtonId::Back, &pan_plan(1000));
    assert_eq!(holds.close_session(&session), Some(HoldCommand::PanEnd));
    assert_eq!(holds.end(&session, ButtonId::Back, DRAG), None);
}

#[test]
fn profile_switch_ends_the_hold_and_the_next_press_can_begin() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    holds.begin(&session, ButtonId::Back, &pan_plan(1000));
    assert_eq!(holds.end_open(&session), Some(HoldCommand::PanEnd));
    assert_eq!(holds.motion(&session, ButtonId::Back, 40, 0), None);
    assert_eq!(holds.end(&session, ButtonId::Back, DRAG), None);
    assert_eq!(
        holds.begin(&session, ButtonId::Back, &pan_plan(1000)),
        Some(HoldCommand::PanBegin)
    );
}

#[test]
fn shutdown_ends_every_open_hold() {
    let mut holds = HoldSessions::default();
    holds.begin(&session(7), ButtonId::Back, &pan_plan(1000));
    holds.begin(&session(8), ButtonId::Forward, &zoom_plan(1000));
    let commands = holds.close_all();
    assert!(commands.contains(&HoldCommand::PanEnd));
    assert!(commands.contains(&HoldCommand::ZoomEnd));
    assert_eq!(holds.motion(&session(7), ButtonId::Back, 20, 0), None);
}

#[test]
fn missing_dpi_or_binding_does_not_open() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let mut plan = pan_plan(1000);
    plan.sensor_dpi = None;
    assert_eq!(holds.begin(&session, ButtonId::Back, &plan), None);

    let mut unbound = pan_plan(1000);
    unbound.hold_bindings.clear();
    assert_eq!(holds.begin(&session, ButtonId::Back, &unbound), None);
    assert_eq!(holds.motion(&session, ButtonId::Back, 50, 0), None);
}

#[test]
fn pan_commands_never_reach_the_zoom_sink() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let plan = pan_plan(1000);
    let mut commands = Vec::new();
    commands.extend(holds.begin(&session, ButtonId::Back, &plan));
    commands.extend(holds.motion(&session, ButtonId::Back, 80, -40));
    commands.extend(holds.end(&session, ButtonId::Back, DRAG));
    assert!(
        commands.iter().all(|command| matches!(
            command,
            HoldCommand::PanBegin | HoldCommand::Pan { .. } | HoldCommand::PanEnd
        )),
        "Pan must not emit Zoom/ZoomEnd: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, HoldCommand::Pan { .. })),
        "the pan sink must actually be reached"
    );
}

#[test]
fn zoom_commands_never_reach_the_pan_sink() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let plan = zoom_plan(1000);
    let mut commands = Vec::new();
    commands.extend(holds.begin(&session, ButtonId::Forward, &plan));
    commands.extend(holds.motion(&session, ButtonId::Forward, 0, -200));
    commands.extend(holds.end(&session, ButtonId::Forward, DRAG));
    assert!(
        commands
            .iter()
            .all(|command| matches!(command, HoldCommand::Zoom { .. } | HoldCommand::ZoomEnd)),
        "Zoom must not emit Pan/PanEnd: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, HoldCommand::Zoom { .. })),
        "the zoom sink must actually be reached"
    );
}

#[test]
fn swapping_the_bound_action_swaps_the_sink() {
    // Same button, same motion: only the hold binding chooses the sink.
    let session = session(7);
    let motion = (0_i16, -200_i16);
    let mut pan = HoldSessions::default();
    pan.begin(&session, ButtonId::Back, &pan_plan(1000));
    let pan_cmd = pan.motion(&session, ButtonId::Back, motion.0, motion.1);
    let mut zoom = HoldSessions::default();
    zoom.begin(&session, ButtonId::Forward, &zoom_plan(1000));
    let zoom_cmd = zoom.motion(&session, ButtonId::Forward, motion.0, motion.1);
    assert!(matches!(pan_cmd, Some(HoldCommand::Pan { .. })));
    assert!(matches!(zoom_cmd, Some(HoldCommand::Zoom { .. })));
    assert_ne!(
        std::mem::discriminant(&pan_cmd.unwrap()),
        std::mem::discriminant(&zoom_cmd.unwrap()),
        "identical raw-XY must not collapse Pan and Zoom into one command"
    );
}

#[test]
fn a_completed_hold_can_begin_again_on_the_same_epoch() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    let plan = pan_plan(1000);
    holds.begin(&session, ButtonId::Back, &plan);
    holds.end(&session, ButtonId::Back, DRAG);
    assert_eq!(
        holds.begin(&session, ButtonId::Back, &plan),
        Some(HoldCommand::PanBegin),
        "a normal button-up returns the epoch to Idle so the next press works"
    );
}

#[test]
fn zoom_sensitivity_scales_the_magnification_rate() {
    let counts = counts_for_mm(-20.0, 1000);
    let dpi = Dpi::new(1000);
    let at_default = zoom_magnification(counts, dpi, ZoomSensitivity::DEFAULT);
    let at_double = zoom_magnification(
        counts,
        dpi,
        ZoomSensitivity::from_rounded(f32::from(ZoomSensitivity::DEFAULT) * 2.0),
    );
    let at_min = zoom_magnification(counts, dpi, ZoomSensitivity::MIN);
    assert!(
        (at_double - at_default * 2.0).abs() < 0.03,
        "twice the sensitivity must zoom twice as fast, got {at_double} vs {at_default}"
    );
    assert!(
        at_min < at_default,
        "the minimum must be slower than the default, got {at_min} vs {at_default}"
    );
    assert!(
        at_min > 0.0,
        "the minimum must still zoom in on an upward drag, got {at_min}"
    );
}

#[test]
fn inverting_pan_flips_both_axes() {
    let session = session(7);
    let mut natural = HoldSessions::default();
    natural.begin(&session, ButtonId::Back, &pan_plan(1000));
    let Some(HoldCommand::Pan { dx, dy }) = natural.motion(&session, ButtonId::Back, 120, -60)
    else {
        panic!("natural pan must emit");
    };

    let mut inverted_plan = pan_plan(1000);
    inverted_plan.invert_pan = true;
    let mut inverted = HoldSessions::default();
    inverted.begin(&session, ButtonId::Back, &inverted_plan);
    let Some(HoldCommand::Pan { dx: idx, dy: idy }) =
        inverted.motion(&session, ButtonId::Back, 120, -60)
    else {
        panic!("inverted pan must emit");
    };

    assert!(
        (idx + dx).abs() < f32::EPSILON && (idy + dy).abs() < f32::EPSILON,
        "inverting must negate both axes: ({dx}, {dy}) became ({idx}, {idy})"
    );
    assert!(
        dx != 0.0 && dy != 0.0,
        "the fixture must move on both axes or the assertion proves nothing"
    );
}

#[test]
fn a_zoom_button_clicked_without_dragging_fires_smart_zoom() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    assert_eq!(
        holds.begin(&session, ButtonId::Forward, &zoom_plan(1000)),
        None,
        "zoom opens on first motion, not on button-down"
    );
    assert_eq!(
        holds.end(&session, ButtonId::Forward, CLICK),
        Some(HoldCommand::SmartZoom)
    );
}

#[test]
fn a_zoom_button_dragged_ends_the_pinch_rather_than_smart_zooming() {
    // The two gestures share one button, which is why smart zoom cannot fire
    // on button-down: it has to wait and see whether a drag follows.
    let mut holds = HoldSessions::default();
    let session = session(7);
    holds.begin(&session, ButtonId::Forward, &zoom_plan(1000));
    assert!(
        holds
            .motion(&session, ButtonId::Forward, 0, -200)
            .is_some_and(|command| matches!(command, HoldCommand::Zoom { .. }))
    );
    assert_eq!(
        holds.end(&session, ButtonId::Forward, DRAG),
        Some(HoldCommand::ZoomEnd)
    );
}

#[test]
fn a_pan_button_clicked_without_dragging_does_not_smart_zoom() {
    let mut holds = HoldSessions::default();
    let session = session(7);
    holds.begin(&session, ButtonId::Back, &pan_plan(1000));
    assert_eq!(
        holds.end(&session, ButtonId::Back, CLICK),
        Some(HoldCommand::PanEnd),
        "smart zoom belongs to the zoom binding only"
    );
}

#[test]
fn tearing_down_a_zoom_hold_never_smart_zooms() {
    // Shutdown, profile switch and stale input close the gesture. None of
    // them is a user clicking the button.
    let session = session(7);
    for command in [
        {
            let mut holds = HoldSessions::default();
            holds.begin(&session, ButtonId::Forward, &zoom_plan(1000));
            holds.close_session(&session)
        },
        {
            let mut holds = HoldSessions::default();
            holds.begin(&session, ButtonId::Forward, &zoom_plan(1000));
            holds.end_open(&session)
        },
        // The one capture actually uses: a reconnect, a capture stop, or the
        // stale bound reaches dispatch as an interrupted HoldEnd, which took
        // the same route as a release and fired a smart zoom into whatever
        // was frontmost.
        {
            let mut holds = HoldSessions::default();
            holds.begin(&session, ButtonId::Forward, &zoom_plan(1000));
            holds.end(&session, ButtonId::Forward, HoldRelease::Interrupted)
        },
    ] {
        assert_eq!(command, Some(HoldCommand::ZoomEnd));
    }
}

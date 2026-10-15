//! What a reporting restore writes and leaves alone.

use super::*;

/// A control that was *already* diverted when the session armed it — an agent
/// killed mid-session, or another Logitech app — must not be handed that state
/// back. Replaying it leaves the button diverted with no listener: no OS event
/// and no HID++ consumer, dead until the device sleeps.
#[test]
fn restore_clears_a_diversion_it_found_already_set() {
    let change = undivert_change(reporting(true, None));

    assert_eq!(change.diverted, Some(false));
    assert_eq!(change.raw_xy, Some(false));
}

/// Arming only ever writes `diverted` / `raw_xy` and re-asserts `remap`, so
/// restoring must leave every other bit alone rather than writing back a
/// snapshot that may itself be this session's leftovers.
#[test]
fn restore_returns_the_remap_target_and_touches_nothing_else() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = undivert_change(reporting(false, Some(remap)));

    assert_eq!(change.remap, Some(remap));
    assert_eq!(change.persistently_diverted, None);
    assert_eq!(change.force_raw_xy, None);
    assert_eq!(change.analytics_key_events, None);
    assert_eq!(change.raw_wheel, None);
}

#[test]
fn wake_rearm_restores_diversion_mode_and_remap_target() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = divert_change(reporting(false, Some(remap)), PointerDivert::RawXy);

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(true));
    assert_eq!(change.remap, Some(remap));
}

#[test]
fn plain_diversion_clears_raw_xy() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = divert_change(reporting(false, Some(remap)), PointerDivert::Plain);

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(false));
    assert_eq!(change.remap, Some(remap));
}

#[test]
fn keyboard_diversion_leaves_raw_xy_unset() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = divert_keyboard_key(reporting(false, Some(remap)));

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, None);
    assert_eq!(change.remap, Some(remap));
}

#[test]
fn keyboard_restore_leaves_raw_xy_unset() {
    let remap = reprog_controls::ControlId(0x0053);

    let change = undivert_keyboard_key(reporting(false, Some(remap)));

    assert_eq!(change.diverted, Some(false));
    assert_eq!(change.raw_xy, None);
    assert_eq!(change.remap, Some(remap));
}

#[test]
fn raw_wheel_diversion_sets_raw_wheel_and_clears_raw_xy() {
    let remap = reprog_controls::ControlId(0x0053);
    let change = divert_change(reporting(false, Some(remap)), PointerDivert::RawWheel);
    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(false));
    assert_eq!(change.raw_wheel, Some(true));
    assert_eq!(change.remap, Some(remap));
}

/// A control without `RAW_WHEEL` has no raw-wheel state to write, so the
/// other modes must leave the bit unset rather than clear it.
#[test]
fn plain_and_raw_xy_diversion_leave_raw_wheel_unset() {
    for divert in [PointerDivert::Plain, PointerDivert::RawXy] {
        assert_eq!(
            divert_change(reporting(false, None), divert).raw_wheel,
            None
        );
        let armed = ArmedReporting::pointer(0x0053, reporting(true, None), divert);
        assert_eq!(armed.undivert_change().raw_wheel, None, "{divert:?}");
    }
}

/// Teardown hands the wheel back even though the control reported raw wheel
/// on before arming: replaying that would leave the wheel unable to scroll.
#[test]
fn a_raw_wheel_control_hands_the_wheel_back_on_restore() {
    let remap = reprog_controls::ControlId(0x0053);
    let armed = ArmedReporting::pointer(
        0x0053,
        reporting(true, Some(remap)),
        PointerDivert::RawWheel,
    );
    let restore = armed.undivert_change();
    assert_eq!(restore.diverted, Some(false));
    assert_eq!(restore.raw_xy, Some(false));
    assert_eq!(restore.raw_wheel, Some(false));
    assert_eq!(restore.remap, Some(remap));
    // A wake rearm re-applies the mode it was armed with.
    assert_eq!(armed.divert_change().raw_wheel, Some(true));
}

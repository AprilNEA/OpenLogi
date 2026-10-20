//! Tests for precision mode and the pointer-speed write order.

use openlogi_core::hid::{DeviceRoute, PointerSpeed};

use super::{PointerSpeeds, ReloadChange, ScaledMouse};

fn route(slot: u8) -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "UNIFYING".to_string(),
        slot,
    }
}

fn speed(raw: u16) -> PointerSpeed {
    PointerSpeed::try_new(raw).expect("a speed in range")
}

fn mouse(key: &str, slot: u8, saved: Option<PointerSpeed>) -> ScaledMouse {
    ScaledMouse {
        key: key.to_string(),
        route: route(slot),
        saved,
    }
}

/// One MX Ergo `a` on slot 1, saved at 1.5×.
fn one_mouse() -> PointerSpeeds {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, Some(speed(0x0180)))]);
    speeds
}

#[test]
fn a_press_turns_precision_on_and_the_next_returns_to_the_saved_speed() {
    let speeds = one_mouse();

    let (on, write) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    assert!(on);
    assert_eq!(write.speed, PointerSpeed::MIN);
    assert_eq!(write.route, route(1));

    let (on, write) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    assert!(!on);
    assert_eq!(write.speed, speed(0x0180));
}

#[test]
fn leaving_precision_with_no_saved_speed_returns_to_one_times() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);

    speeds.toggle_precision(Some("a"));
    let (_, write) = speeds.toggle_precision(Some("a")).expect("an online mouse");

    assert_eq!(write.speed, PointerSpeed::DEFAULT);
}

#[test]
fn a_press_without_a_device_acts_on_the_selected_one() {
    let speeds = PointerSpeeds::default();
    speeds.publish(Some("b"), [mouse("a", 1, None), mouse("b", 2, None)]);

    let (_, write) = speeds.toggle_precision(None).expect("the selected mouse");

    assert_eq!(write.route, route(2));
    assert!(speeds.precision("b") && !speeds.precision("a"));
}

#[test]
fn a_press_for_a_mouse_without_pointer_scaling_does_nothing() {
    let speeds = one_mouse();

    assert!(speeds.toggle_precision(Some("keyboard")).is_none());
    assert!(PointerSpeeds::default().toggle_precision(None).is_none());
}

#[test]
fn precision_survives_a_reconnect_on_a_new_route() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"));

    // Offline: the mode is kept.
    speeds.publish(None, []);
    assert!(speeds.precision("a"));
    // Back on another slot: the reapply writes the precision speed there.
    let write = speeds
        .reapply("a", &route(3), Some(speed(0x0180)))
        .expect("precision is on");
    assert_eq!(write.speed, PointerSpeed::MIN);
    assert_eq!(write.route, route(3));
}

#[test]
fn a_reconnect_without_precision_writes_only_a_saved_speed() {
    let speeds = one_mouse();

    let write = speeds
        .reapply("a", &route(1), Some(speed(0x0180)))
        .expect("a saved speed");
    assert_eq!(write.speed, speed(0x0180));
    assert!(speeds.reapply("a", &route(1), None).is_none());
}

#[test]
fn a_write_from_the_app_ends_precision_and_becomes_the_speed_to_return_to() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"));

    let _ticket = speeds.app_write(&route(1), speed(0x0200));

    assert!(!speeds.precision("a"));
    let (on, write) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    assert!(on, "the next press turns precision on again");
    let (_, write_off) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    assert_eq!(write.speed, PointerSpeed::MIN);
    assert_eq!(write_off.speed, speed(0x0200));
}

#[test]
fn a_reload_that_changes_assignments_ends_precision() {
    let speeds = one_mouse();
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };
    // Precision off: nothing to undo.
    assert!(
        speeds
            .reload("a", &route(1), Some(speed(0x0180)), assignments)
            .is_none()
    );

    speeds.toggle_precision(Some("a"));
    let write = speeds
        .reload("a", &route(1), None, assignments)
        .expect("precision ended");

    assert!(!speeds.precision("a"));
    assert_eq!(write.speed, PointerSpeed::DEFAULT);
}

#[test]
fn a_reload_that_changes_the_speed_writes_it_and_ends_precision() {
    let speeds = one_mouse();
    let changed = ReloadChange {
        speed: true,
        ..ReloadChange::default()
    };

    let write = speeds
        .reload("a", &route(1), Some(speed(0x0080)), changed)
        .expect("a new speed");
    assert_eq!(write.speed, speed(0x0080));
    // A removed speed leaves the mouse alone.
    assert!(speeds.reload("a", &route(1), None, changed).is_none());

    speeds.toggle_precision(Some("a"));
    let write = speeds
        .reload("a", &route(1), Some(speed(0x0080)), changed)
        .expect("precision ended");
    assert!(!speeds.precision("a"));
    assert_eq!(write.speed, speed(0x0080));
}

#[tokio::test]
async fn the_newest_press_supersedes_a_write_still_waiting() {
    let speeds = one_mouse();
    let (_, on) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    let (_, off) = speeds.toggle_precision(Some("a")).expect("an online mouse");

    let older = on
        .ticket
        .write_or(|| async { "wrote" }, || async { "skipped" })
        .await;
    let newer = off
        .ticket
        .write_or(|| async { "wrote" }, || async { "skipped" })
        .await;

    assert_eq!((older, newer), ("skipped", "wrote"));
}

//! Tests for precision mode and the pointer-speed write order.

use openlogi_core::hid::{DeviceRoute, PointerSpeed};

use super::{PointerSpeeds, ReloadChange, ScaledMouse, SpeedWrite};

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

/// A reload that changed `change` for every config key.
fn every_key(change: ReloadChange) -> impl Fn(&str) -> ReloadChange {
    move |_| change
}

/// The speeds `writes` put on the mice, in order.
fn written(writes: &[SpeedWrite]) -> Vec<PointerSpeed> {
    writes.iter().map(|write| write.speed).collect()
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
            .reload(every_key(assignments), [mouse("a", 1, Some(speed(0x0180)))])
            .is_empty()
    );

    speeds.toggle_precision(Some("a"));
    let writes = speeds.reload(every_key(assignments), [mouse("a", 1, None)]);

    assert!(!speeds.precision("a"));
    assert_eq!(written(&writes), [PointerSpeed::DEFAULT]);
}

#[test]
fn a_reload_that_changes_the_speed_writes_it_and_ends_precision() {
    let speeds = one_mouse();
    let changed = every_key(ReloadChange {
        speed: true,
        ..ReloadChange::default()
    });

    let writes = speeds.reload(&changed, [mouse("a", 1, Some(speed(0x0080)))]);
    assert_eq!(written(&writes), [speed(0x0080)]);
    // A removed speed leaves the mouse alone.
    assert!(speeds.reload(&changed, [mouse("a", 1, None)]).is_empty());

    speeds.toggle_precision(Some("a"));
    let writes = speeds.reload(&changed, [mouse("a", 1, Some(speed(0x0080)))]);
    assert!(!speeds.precision("a"));
    assert_eq!(written(&writes), [speed(0x0080)]);
}

#[test]
fn a_reload_ends_precision_on_an_offline_mouse() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"));
    // `a` disconnects, and its buttons are rebound while it is away.
    speeds.publish(None, []);
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };

    let writes = speeds.reload(every_key(assignments), []);

    assert!(writes.is_empty(), "an offline mouse gets no write");
    assert!(!speeds.precision("a"));
    // It reconnects at its saved speed, not at the precision speed.
    let write = speeds
        .reapply("a", &route(1), Some(speed(0x0180)))
        .expect("a saved speed");
    assert_eq!(write.speed, speed(0x0180));
}

/// A mouse in precision mode goes offline, and a reload that changes its
/// assignments ends the mode.
fn offline_mouse_whose_precision_a_reload_ended() -> PointerSpeeds {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"));
    speeds.publish(None, []);
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };
    assert!(speeds.reload(every_key(assignments), []).is_empty());
    speeds
}

/// The mouse left at the precision speed, so its reconnect writes 1× even
/// with no speed saved. Once that write lands, nothing more is owed.
#[test]
fn an_offline_mouse_with_no_saved_speed_reconnects_at_one_times() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();

    let write = speeds
        .reapply("a", &route(2), None)
        .expect("the mouse may still move at the precision speed");
    assert_eq!(write.speed, PointerSpeed::DEFAULT);
    assert_eq!(write.route, route(2));
    speeds.restored(&write.restore.expect("the write settles the restore"));

    assert!(!speeds.restore_pending("a"));
    assert!(speeds.reapply("a", &route(2), None).is_none());
}

/// A restore write that times out or fails settles nothing, so the next
/// reconnect pass writes 1× again.
#[test]
fn a_restore_stays_pending_until_its_write_lands() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();

    let first = speeds.reapply("a", &route(2), None).expect("a restore");
    let second = speeds.reapply("a", &route(2), None).expect("still pending");
    assert_eq!(second.speed, PointerSpeed::DEFAULT);
    assert_eq!(first.restore, second.restore);
    assert!(speeds.restore_pending("a"));
}

/// A restore write that lands after a newer reload replaced the entry does
/// not clear the newer entry.
#[test]
fn a_late_restore_write_keeps_a_newer_pending_restore() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();
    let stale = speeds
        .reapply("a", &route(2), None)
        .and_then(|write| write.restore)
        .expect("a restore");

    // The mouse comes back, enters precision mode, and leaves again before
    // the old write lands; another reload ends the mode offline.
    speeds.publish(None, [mouse("a", 2, None)]);
    speeds.toggle_precision(Some("a"));
    speeds.publish(None, []);
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };
    assert!(speeds.reload(every_key(assignments), []).is_empty());

    speeds.restored(&stale);
    assert!(speeds.restore_pending("a"));
}

/// A press or a write from the app replaces the pending restore with a
/// write of its own.
#[test]
fn a_press_or_an_app_write_drops_the_pending_restore() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();
    speeds.publish(None, [mouse("a", 2, None)]);
    speeds.toggle_precision(Some("a"));
    assert!(!speeds.restore_pending("a"));

    let speeds = offline_mouse_whose_precision_a_reload_ended();
    speeds.publish(None, [mouse("a", 2, None)]);
    speeds.app_write(&route(2), speed(0x0180));
    assert!(!speeds.restore_pending("a"));
}

/// A press that ends precision mode owes the mouse its speed until that
/// write lands. A mouse that sleeps before it lands gets 1× at its
/// reconnect.
#[test]
fn a_press_that_ends_precision_owes_the_speed_until_its_write_lands() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"));
    let (on, off) = speeds.toggle_precision(Some("a")).expect("an online mouse");
    assert!(!on);
    let restore = off.restore.expect("the press owes a restore");

    let write = speeds.reapply("a", &route(1), None).expect("still owed");
    assert_eq!(write.speed, PointerSpeed::DEFAULT);
    speeds.restored(&restore);
    assert!(speeds.reapply("a", &route(1), None).is_none());
}

/// The same holds for a reload that ends precision mode on an online mouse.
#[test]
fn an_online_reload_that_ends_precision_owes_the_speed_until_its_write_lands() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"));
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };

    let mut writes = speeds.reload(every_key(assignments), [mouse("a", 1, None)]);
    let write = writes.pop().expect("a write to 1×");
    assert!(speeds.restore_pending("a"));
    speeds.restored(&write.restore.expect("the reload owes a restore"));
    assert!(!speeds.restore_pending("a"));
}

/// A reload that removes the saved speed of a mouse that may still be slow
/// writes 1×, and that write settles the restore.
#[test]
fn a_reload_that_removes_the_speed_writes_one_times_while_a_restore_is_pending() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();
    let changed = every_key(ReloadChange {
        speed: true,
        ..ReloadChange::default()
    });

    let writes = speeds.reload(&changed, [mouse("a", 2, None)]);

    assert_eq!(written(&writes), [PointerSpeed::DEFAULT]);
    assert!(writes[0].restore.is_some());
}

#[test]
fn a_reload_keeps_precision_on_the_mice_it_did_not_change() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None), mouse("b", 2, None)]);
    speeds.toggle_precision(Some("a"));
    speeds.toggle_precision(Some("b"));
    // `a` disconnects. The reload rebinds only `b`.
    speeds.publish(None, [mouse("b", 2, None)]);
    let only_b = |key: &str| ReloadChange {
        assignments: key == "b",
        ..ReloadChange::default()
    };

    let writes = speeds.reload(only_b, [mouse("b", 2, None)]);

    assert!(speeds.precision("a"));
    assert!(!speeds.precision("b"));
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].route, route(2));
    assert_eq!(writes[0].speed, PointerSpeed::DEFAULT);
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

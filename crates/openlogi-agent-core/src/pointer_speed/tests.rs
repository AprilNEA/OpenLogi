//! Tests for precision mode and the pointer-speed write order.

use openlogi_core::hid::{DeviceRoute, Dpi, PointerSpeed};
use openlogi_hid::DpiTarget;

use super::{
    AppWrite, PointerSpeeds, PrecisionPress, ReloadChange, SavedSpeed, SpeedMouse, SpeedTarget,
    SpeedWrite,
};

fn route(slot: u8) -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "UNIFYING".to_string(),
        slot,
    }
}

fn speed(raw: u16) -> PointerSpeed {
    PointerSpeed::try_new(raw).expect("a speed in range")
}

fn mouse(key: &str, slot: u8, saved: Option<PointerSpeed>) -> SpeedMouse {
    SpeedMouse {
        key: key.to_string(),
        route: route(slot),
        saved: SavedSpeed::Scaled(saved),
    }
}

/// A DPI mouse on `slot`, with `saved` as its saved DPI.
fn dpi_mouse(key: &str, slot: u8, saved: Option<Dpi>) -> SpeedMouse {
    SpeedMouse {
        key: key.to_string(),
        route: route(slot),
        saved: SavedSpeed::Dpi(saved),
    }
}

const fn dpi(target: DpiTarget) -> SpeedTarget {
    SpeedTarget::Dpi(target)
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

/// What `writes` put on the mice, in order.
fn written(writes: &[SpeedWrite]) -> Vec<SpeedTarget> {
    writes.iter().map(|write| write.target).collect()
}

#[test]
fn a_press_turns_precision_on_and_the_next_returns_to_the_saved_speed() {
    let speeds = one_mouse();

    let (on, write) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    assert!(on);
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::MIN));
    assert_eq!(write.route, route(1));

    let (on, write) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    assert!(!on);
    assert_eq!(write.target, SpeedTarget::Scaled(speed(0x0180)));
}

#[test]
fn leaving_precision_with_no_saved_speed_returns_to_one_times() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);

    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let (_, write) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");

    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
}

#[test]
fn a_press_without_a_device_acts_on_the_selected_one() {
    let speeds = PointerSpeeds::default();
    speeds.publish(Some("b"), [mouse("a", 1, None), mouse("b", 2, None)]);

    let (_, write) = speeds
        .toggle_precision(None, PrecisionPress::Assigned)
        .expect("the selected mouse");

    assert_eq!(write.route, route(2));
    assert!(speeds.precision("b") && !speeds.precision("a"));
}

#[test]
fn a_press_for_a_mouse_without_pointer_scaling_does_nothing() {
    let speeds = one_mouse();

    assert!(
        speeds
            .toggle_precision(Some("keyboard"), PrecisionPress::Assigned)
            .is_none()
    );
    assert!(
        PointerSpeeds::default()
            .toggle_precision(None, PrecisionPress::Assigned)
            .is_none()
    );
}

#[test]
fn precision_survives_a_reconnect_on_a_new_route() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);

    // Offline: the mode is kept.
    speeds.publish(None, []);
    assert!(speeds.precision("a"));
    // Back on another slot: the reapply writes the precision speed there.
    let write = speeds
        .reapply("a", &route(3), SavedSpeed::Scaled(Some(speed(0x0180))))
        .expect("precision is on");
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::MIN));
    assert_eq!(write.route, route(3));
}

#[test]
fn a_reconnect_without_precision_writes_only_a_saved_speed() {
    let speeds = one_mouse();

    let write = speeds
        .reapply("a", &route(1), SavedSpeed::Scaled(Some(speed(0x0180))))
        .expect("a saved speed");
    assert_eq!(write.target, SpeedTarget::Scaled(speed(0x0180)));
    assert!(
        speeds
            .reapply("a", &route(1), SavedSpeed::Scaled(None))
            .is_none()
    );
}

#[test]
fn a_write_from_the_app_ends_precision_and_becomes_the_speed_to_return_to() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);

    let _ticket = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0200))));

    assert!(!speeds.precision("a"));
    let (on, write) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    assert!(on, "the next press turns precision on again");
    let (_, write_off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::MIN));
    assert_eq!(write_off.target, SpeedTarget::Scaled(speed(0x0200)));
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

    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let writes = speeds.reload(every_key(assignments), [mouse("a", 1, None)]);

    assert!(!speeds.precision("a"));
    assert_eq!(
        written(&writes),
        [SpeedTarget::Scaled(PointerSpeed::DEFAULT)]
    );
}

#[test]
fn a_reload_that_changes_the_speed_writes_it_and_ends_precision() {
    let speeds = one_mouse();
    let changed = every_key(ReloadChange {
        speed: true,
        ..ReloadChange::default()
    });

    let writes = speeds.reload(&changed, [mouse("a", 1, Some(speed(0x0080)))]);
    assert_eq!(written(&writes), [SpeedTarget::Scaled(speed(0x0080))]);
    // A removed speed leaves the mouse alone.
    assert!(speeds.reload(&changed, [mouse("a", 1, None)]).is_empty());

    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let writes = speeds.reload(&changed, [mouse("a", 1, Some(speed(0x0080)))]);
    assert!(!speeds.precision("a"));
    assert_eq!(written(&writes), [SpeedTarget::Scaled(speed(0x0080))]);
}

#[test]
fn a_reload_ends_precision_on_an_offline_mouse() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
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
        .reapply("a", &route(1), SavedSpeed::Scaled(Some(speed(0x0180))))
        .expect("a saved speed");
    assert_eq!(write.target, SpeedTarget::Scaled(speed(0x0180)));
}

/// A mouse in precision mode goes offline, and a reload that changes its
/// assignments ends the mode.
fn offline_mouse_whose_precision_a_reload_ended() -> PointerSpeeds {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
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
        .reapply("a", &route(2), SavedSpeed::Scaled(None))
        .expect("the mouse may still move at the precision speed");
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
    assert_eq!(write.route, route(2));
    speeds.restored(&write.restore.expect("the write settles the restore"));

    assert!(!speeds.restore_pending("a"));
    assert!(
        speeds
            .reapply("a", &route(2), SavedSpeed::Scaled(None))
            .is_none()
    );
}

/// A restore write that times out or fails settles nothing, so the next
/// reconnect pass writes 1× again.
#[test]
fn a_restore_stays_pending_until_its_write_lands() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();

    let first = speeds
        .reapply("a", &route(2), SavedSpeed::Scaled(None))
        .expect("a restore");
    let second = speeds
        .reapply("a", &route(2), SavedSpeed::Scaled(None))
        .expect("still pending");
    assert_eq!(second.target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
    assert_eq!(first.restore, second.restore);
    assert!(speeds.restore_pending("a"));
}

/// A restore write that lands after a newer reload replaced the entry does
/// not clear the newer entry.
#[test]
fn a_late_restore_write_keeps_a_newer_pending_restore() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();
    let stale = speeds
        .reapply("a", &route(2), SavedSpeed::Scaled(None))
        .and_then(|write| write.restore)
        .expect("a restore");

    // The mouse comes back, enters precision mode, and leaves again before
    // the old write lands; another reload ends the mode offline.
    speeds.publish(None, [mouse("a", 2, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    speeds.publish(None, []);
    let assignments = ReloadChange {
        assignments: true,
        ..ReloadChange::default()
    };
    assert!(speeds.reload(every_key(assignments), []).is_empty());

    speeds.restored(&stale);
    assert!(speeds.restore_pending("a"));
}

/// A press drops the pending restore: it sets the precision speed again. A
/// write from the app takes the restore over and settles it once it lands.
#[test]
fn a_press_drops_the_pending_restore_and_an_app_write_takes_it_over() {
    let speeds = offline_mouse_whose_precision_a_reload_ended();
    speeds.publish(None, [mouse("a", 2, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    assert!(!speeds.restore_pending("a"));

    let speeds = offline_mouse_whose_precision_a_reload_ended();
    speeds.publish(None, [mouse("a", 2, None)]);
    let app = speeds.app_write(&route(2), SavedSpeed::Scaled(Some(speed(0x0180))));
    assert!(speeds.restore_pending("a"), "until the app write lands");

    speeds.restored(&app.restore.expect("the app write ends the slow speed"));
    assert!(!speeds.restore_pending("a"));
}

/// A press that ends precision mode owes the mouse its speed until that
/// write lands. A mouse that sleeps before it lands gets 1× at its
/// reconnect.
#[test]
fn a_press_that_ends_precision_owes_the_speed_until_its_write_lands() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let (on, off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    assert!(!on);
    let restore = off.restore.expect("the press owes a restore");

    let write = speeds
        .reapply("a", &route(1), SavedSpeed::Scaled(None))
        .expect("still owed");
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
    speeds.restored(&restore);
    assert!(
        speeds
            .reapply("a", &route(1), SavedSpeed::Scaled(None))
            .is_none()
    );
}

/// The same holds for a reload that ends precision mode on an online mouse.
#[test]
fn an_online_reload_that_ends_precision_owes_the_speed_until_its_write_lands() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
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

    assert_eq!(
        written(&writes),
        [SpeedTarget::Scaled(PointerSpeed::DEFAULT)]
    );
    assert!(writes[0].restore.is_some());
}

#[test]
fn a_reload_keeps_precision_on_the_mice_it_did_not_change() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None), mouse("b", 2, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    speeds.toggle_precision(Some("b"), PrecisionPress::Assigned);
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
    assert_eq!(writes[0].target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
}

#[tokio::test]
async fn the_newest_press_supersedes_a_write_still_waiting() {
    let speeds = one_mouse();
    let (_, on) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    let (_, off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");

    let older = on
        .ticket
        .write_or(
            std::future::ready(()),
            || async { "wrote" },
            || async { "skipped" },
        )
        .await;
    let newer = off
        .ticket
        .write_or(
            std::future::ready(()),
            || async { "wrote" },
            || async { "skipped" },
        )
        .await;

    assert_eq!((older, newer), ("skipped", "wrote"));
}

#[test]
fn a_dpi_mouse_moves_at_its_lowest_dpi_in_precision_mode_and_returns_to_its_saved_dpi() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, Some(Dpi::new(1600)))]);

    let (on, write) = speeds
        .toggle_precision(Some("m"), PrecisionPress::Assigned)
        .expect("an online DPI mouse");
    assert!(on);
    assert_eq!(write.target, dpi(DpiTarget::Lowest));

    let (on, write) = speeds
        .toggle_precision(Some("m"), PrecisionPress::Assigned)
        .expect("an online DPI mouse");
    assert!(!on);
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(1600))));
    assert!(write.restore.is_some());
}

#[test]
fn a_dpi_mouse_with_no_saved_dpi_returns_to_its_default_dpi() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, None)]);
    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);

    let (_, write) = speeds
        .toggle_precision(Some("m"), PrecisionPress::Assigned)
        .expect("an online DPI mouse");
    assert_eq!(write.target, dpi(DpiTarget::Default));
}

#[test]
fn the_dpi_button_toggles_precision_mode_only_on_a_scaled_mouse() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None), dpi_mouse("m", 2, None)]);

    assert!(
        speeds
            .toggle_precision(Some("m"), PrecisionPress::DpiButton)
            .is_none()
    );
    assert!(!speeds.precision("m"));
    assert!(
        speeds
            .toggle_precision(Some("a"), PrecisionPress::DpiButton)
            .is_some()
    );
    assert!(speeds.precision("a"));
}

#[test]
fn a_dpi_mouse_reconnects_at_its_lowest_dpi_in_precision_mode_and_at_its_saved_dpi_otherwise() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, Some(Dpi::new(1600)))]);
    let saved = SavedSpeed::Dpi(Some(Dpi::new(1600)));
    let write = speeds.reapply("m", &route(2), saved).expect("a saved DPI");
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(1600))));
    assert!(write.restore.is_none());

    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);
    let write = speeds.reapply("m", &route(2), saved).expect("precision on");
    assert_eq!(write.target, dpi(DpiTarget::Lowest));

    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);
    let write = speeds
        .reapply("m", &route(2), saved)
        .expect("a restore owed");
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(1600))));
    let restore = write.restore.expect("the restore rides on the write");
    speeds.restored(&restore);
    let write = speeds.reapply("m", &route(2), saved).expect("a saved DPI");
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(1600))));
    assert!(write.restore.is_none());
}

#[test]
fn a_dpi_mouse_with_no_saved_dpi_gets_no_reconnect_write_outside_precision_mode() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, None)]);

    assert!(
        speeds
            .reapply("m", &route(2), SavedSpeed::Dpi(None))
            .is_none()
    );
}

/// Whether `write` still writes when its turn comes.
fn takes_its_turn(write: &SpeedWrite) -> bool {
    futures_lite::future::block_on(write.ticket.write_or(
        std::future::ready(()),
        || async { true },
        || async { false },
    ))
}

#[test]
fn a_press_supersedes_a_reconnect_write_still_waiting_for_the_mouse() {
    for mouse in [
        dpi_mouse("m", 2, Some(Dpi::new(1600))),
        self::mouse("m", 2, Some(speed(0x0180))),
    ] {
        let speeds = PointerSpeeds::default();
        let saved = mouse.saved;
        speeds.publish(None, [mouse]);
        let reconnect = speeds
            .reapply("m", &route(2), saved)
            .expect("a saved value");

        let (on, press) = speeds
            .toggle_precision(Some("m"), PrecisionPress::Assigned)
            .expect("an online mouse");

        assert!(on);
        assert!(!takes_its_turn(&reconnect), "{saved:?}: the press is newer");
        assert!(takes_its_turn(&press));
    }
}

#[test]
fn a_dpi_written_by_the_app_ends_precision_mode_only_on_a_dpi_mouse() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [mouse("a", 1, None), dpi_mouse("m", 2, None)]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);

    let _ticket = speeds.app_write(&route(1), SavedSpeed::Dpi(Some(Dpi::new(800))));
    assert!(
        speeds.precision("a"),
        "a DPI write is not a scaled mouse's speed"
    );
    let _ticket = speeds.app_write(&route(2), SavedSpeed::Dpi(Some(Dpi::new(800))));
    assert!(!speeds.precision("m"));

    // The app's DPI is what precision mode now returns to.
    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);
    let (_, write) = speeds
        .toggle_precision(Some("m"), PrecisionPress::Assigned)
        .expect("an online DPI mouse");
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(800))));
}

#[test]
fn a_preset_ends_precision_mode_and_carries_the_restore_it_owes() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, Some(Dpi::new(1600)))]);
    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);

    let write = speeds.preset_write(&route(2), Dpi::new(800));
    assert!(!speeds.precision("m"));
    assert_eq!(write.target, dpi(DpiTarget::Value(Dpi::new(800))));
    let restore = write.restore.expect("the preset settles the restore");
    assert!(speeds.restore_pending("m"));
    speeds.restored(&restore);
    assert!(!speeds.restore_pending("m"));

    // A preset for a mouse precision mode does not set carries nothing.
    assert!(
        speeds
            .preset_write(&route(5), Dpi::new(800))
            .restore
            .is_none()
    );
}

#[test]
fn a_reload_with_a_new_dpi_writes_only_a_dpi_mouse_that_is_owed_a_restore() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("m", 2, Some(Dpi::new(1600)))]);
    let new_dpi = every_key(ReloadChange {
        speed: true,
        assignments: false,
    });

    // The settings app already wrote the new DPI: the reload writes nothing.
    assert!(
        speeds
            .reload(&new_dpi, [dpi_mouse("m", 2, Some(Dpi::new(1600)))])
            .is_empty()
    );

    speeds.toggle_precision(Some("m"), PrecisionPress::Assigned);
    let writes = speeds.reload(&new_dpi, [dpi_mouse("m", 2, Some(Dpi::new(1600)))]);
    assert!(!speeds.precision("m"));
    assert_eq!(written(&writes), [dpi(DpiTarget::Value(Dpi::new(1600)))]);
}

/// What `writes` show on the DPI LED, in order.
fn leds(writes: &[SpeedWrite]) -> Vec<Option<bool>> {
    writes.iter().map(|write| write.led).collect()
}

#[test]
fn a_press_lights_the_led_and_the_next_darkens_it() {
    let speeds = one_mouse();

    let (_, on) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");
    let (_, off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse");

    assert_eq!(leds(&[on, off]), [Some(true), Some(false)]);
}

#[test]
fn a_reconnect_shows_the_mode_on_the_led_and_leaves_it_otherwise() {
    let speeds = one_mouse();
    let saved = SavedSpeed::Scaled(Some(speed(0x0180)));
    let plain = speeds
        .reapply("a", &route(1), saved)
        .expect("a saved speed");
    assert_eq!(plain.led, None, "a mouse that never used precision mode");

    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let on = speeds
        .reapply("a", &route(1), saved)
        .expect("precision mode");
    assert_eq!(on.led, Some(true));

    // The restore write of this press has not landed yet.
    let _off = speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let owed = speeds
        .reapply("a", &route(1), saved)
        .expect("a restore is owed");
    assert_eq!(
        owed.led,
        Some(false),
        "the write that ends the precision speed"
    );
}

#[test]
fn app_writes_end_the_precision_speed_until_one_lands() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);

    let first = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0200))));
    let second = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0220))));
    assert!(first.restore.is_some());
    assert!(
        second.restore.is_some(),
        "the first write has not landed: the mouse may still be slow"
    );
    assert!(speeds.restore_pending("a"));

    speeds.restored(&second.restore.expect("a restore"));
    assert!(!speeds.restore_pending("a"));
    let third = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0240))));
    assert!(third.restore.is_none());
}

#[test]
fn a_late_restore_of_an_older_app_write_leaves_the_newer_one_owed() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let first = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0200))));
    let _second = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0220))));

    speeds.restored(&first.restore.expect("a restore"));

    assert!(speeds.restore_pending("a"));
}

#[test]
fn a_preset_that_supersedes_an_app_write_darkens_the_led_in_its_place() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("a", 1, Some(Dpi::new(1600)))]);
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let app = speeds.app_write(&route(1), SavedSpeed::Dpi(Some(Dpi::new(1200))));
    assert!(app.restore.is_some());

    let preset = speeds.preset_write(&route(1), Dpi::new(800));

    assert!(!takes_its_turn_app(&app), "the preset is newer");
    assert_eq!(preset.led, Some(false));
    assert!(preset.restore.is_some(), "the preset settles the restore");
}

#[test]
fn a_reconnect_after_an_app_write_that_never_landed_darkens_the_led() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let _app = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0200))));

    let write = speeds
        .reapply("a", &route(1), SavedSpeed::Scaled(Some(speed(0x0200))))
        .expect("a restore is owed");

    assert_eq!(write.target, SpeedTarget::Scaled(speed(0x0200)));
    assert_eq!(write.led, Some(false));
    assert!(write.restore.is_some());
}

/// Whether the app write still writes when its turn comes.
fn takes_its_turn_app(write: &AppWrite) -> bool {
    futures_lite::future::block_on(write.ticket.write_or(
        std::future::ready(()),
        || async { true },
        || async { false },
    ))
}

#[test]
fn an_app_write_after_a_press_that_ended_the_mode_still_darkens_the_led() {
    let speeds = one_mouse();
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    // The restore write of this press has not landed yet.
    let _off = speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);

    let write = speeds.app_write(&route(1), SavedSpeed::Scaled(Some(speed(0x0200))));

    assert!(write.restore.is_some());
}

#[test]
fn a_preset_darkens_the_led_only_when_it_ends_the_precision_speed() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("a", 1, Some(Dpi::new(1600)))]);
    let plain = speeds.preset_write(&route(1), Dpi::new(800));
    assert_eq!(plain.led, None);

    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);
    let ending = speeds.preset_write(&route(1), Dpi::new(800));
    assert_eq!(ending.led, Some(false));
}

#[test]
fn a_reload_darkens_the_led_of_the_mice_whose_mode_it_ends() {
    let speeds = PointerSpeeds::default();
    speeds.publish(
        None,
        [
            mouse("a", 1, Some(speed(0x0180))),
            mouse("b", 2, Some(speed(0x0200))),
        ],
    );
    speeds.toggle_precision(Some("a"), PrecisionPress::Assigned);

    let writes = speeds.reload(
        every_key(ReloadChange {
            speed: true,
            assignments: false,
        }),
        [
            mouse("a", 1, Some(speed(0x0180))),
            mouse("b", 2, Some(speed(0x0220))),
        ],
    );

    assert_eq!(leds(&writes), [Some(false), None]);
}

/// A DPI shift's restore waits out precision mode: no place while the mode
/// is on, nor while a newer end of it is owed, so neither write is skipped.
#[test]
fn a_dpi_shift_restore_takes_no_place_from_precision_mode() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("a", 1, Some(Dpi::new(1600)))]);
    let restore = speeds
        .shift_restore("a", &route(1), None)
        .expect("nothing owed");
    assert_eq!(restore.restore, None);

    speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("precision on");
    assert!(speeds.shift_restore("a", &route(1), None).is_none());

    let (on, off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("precision off");
    assert!(!on);
    assert!(speeds.shift_restore("a", &route(1), None).is_none());
    speeds.restored(off.restore.as_ref().expect("the end owes a restore"));
    assert!(speeds.shift_restore("a", &route(1), None).is_some());
}

/// A shift that began in precision mode ended it with its own low write. A
/// quick release must still restore, and takes over the restore that write
/// owed, since its place skips the write if it still waits. A newer end of
/// precision mode keeps the release out.
#[test]
fn a_dpi_shift_restore_takes_over_its_own_end_of_precision_mode() {
    let speeds = PointerSpeeds::default();
    speeds.publish(None, [dpi_mouse("a", 1, Some(Dpi::new(1600)))]);
    speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("precision on");
    let shift = speeds.preset_write(&route(1), Dpi::new(400));
    let own = shift
        .restore
        .clone()
        .expect("the shift ends precision mode");

    let restore = speeds
        .shift_restore("a", &route(1), Some(&own))
        .expect("the release restores");
    assert_eq!(restore.restore, Some(own.clone()));

    speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("precision on again");
    let (_, off) = speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("precision off again");
    assert_ne!(off.restore.as_ref(), Some(&own));
    assert!(speeds.shift_restore("a", &route(1), Some(&own)).is_none());
}

/// Entering host mode again owes the write a reconnect owes: the saved DPI,
/// or the precision DPI while the mode is on; nothing with no DPI saved.
#[test]
fn host_mode_owes_the_reconnect_write() {
    let speeds = PointerSpeeds::default();
    speeds.publish(
        None,
        [
            dpi_mouse("saved", 1, Some(Dpi::new(1200))),
            dpi_mouse("unsaved", 2, None),
        ],
    );
    let write = speeds.host_mode_write("saved").expect("a saved DPI");
    assert_eq!(write.dpi(), Some(Dpi::new(1200)));
    assert_eq!(write.route, route(1));
    assert!(speeds.host_mode_write("unsaved").is_none());
    assert!(speeds.host_mode_write("offline").is_none());

    speeds
        .toggle_precision(Some("saved"), PrecisionPress::Assigned)
        .expect("precision on");
    let write = speeds.host_mode_write("saved").expect("the precision DPI");
    assert_eq!(write.target, dpi(DpiTarget::Lowest));
    assert_eq!(write.dpi(), None);
}

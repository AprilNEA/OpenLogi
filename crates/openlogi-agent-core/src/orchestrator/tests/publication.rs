//! Inventory readiness and what every mutator republishes.

use super::*;

/// An *empty* snapshot still flips the health to `Ready`: the watcher only
/// forwards completed enumerations, so "checked and found nothing" must not
/// be reported as "still scanning" — that's the whole distinction the
/// health exists to carry.
#[test]
fn empty_refresh_marks_inventory_ready() {
    let mut orch = orchestrator(Config::default());
    assert_eq!(orch.inventory_health(), InventoryHealth::Scanning);
    orch.refresh_inventory(&[], &[], false);
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
}

/// `Unavailable` is a startup-only downgrade: it reports "enumeration has
/// never worked", recovers when a snapshot finally lands, and never
/// clobbers a live device set on a mid-session failure (mirroring the
/// watcher's keep-last-snapshot policy).
#[test]
fn unavailable_only_downgrades_a_pending_inventory() {
    let mut orch = orchestrator(Config::default());
    orch.mark_inventory_unavailable();
    assert_eq!(orch.inventory_health(), InventoryHealth::Unavailable);
    orch.refresh_inventory(&[], &[], false);
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
    orch.mark_inventory_unavailable();
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
}

#[test]
fn every_inventory_mutator_republishes_what_the_ipc_server_answers() {
    let observable = Arc::new(ObservableState::new("test".to_string()));
    let mut orch = Orchestrator::new(Config::default(), Arc::clone(&observable));
    assert_eq!(
        observable.snapshot().status.inventory,
        InventoryHealth::Scanning,
        "a fresh agent has not enumerated yet"
    );

    orch.mark_inventory_unavailable();
    assert_eq!(
        observable.snapshot().status.inventory,
        InventoryHealth::Unavailable
    );

    orch.refresh_inventory(
        &[direct_inventory(Some("serial-1"), [1, 2, 3, 4])],
        &[],
        false,
    );
    let published = observable.snapshot();
    assert_eq!(published.status.inventory, InventoryHealth::Ready);
    assert_eq!(published.inventory, orch.inventory());
    assert_eq!(published.standalone, orch.standalone());
    assert_eq!(published.inventory.len(), 1);

    // A camera sample and a config reload are the other two facts the cell
    // carries; both must reach it from inside the mutator.
    orch.set_camera_active(true);
    assert!(observable.snapshot().camera_active);

    let mut config = Config::default();
    config.app_settings.launch_at_login = true;
    orch.reload_config(config);
    assert!(observable.snapshot().status.launch_at_login);
}

#[test]
fn equal_runtime_projection_does_not_wake_managers() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    let mut capture_plans = orch.shared.capture_plans.clone();
    let mut keyboard_spec = orch.shared.keyboard_spec.clone();
    let mut host_switch_links = orch.shared.host_switch_links.clone();
    let _ = capture_plans.borrow_and_update();
    let _ = keyboard_spec.borrow_and_update();
    let _ = host_switch_links.borrow_and_update();

    orch.publish_device_runtime();

    assert!(
        !capture_plans
            .has_changed()
            .expect("publication remains open")
    );
    assert!(
        !keyboard_spec
            .has_changed()
            .expect("publication remains open")
    );
    assert!(
        !host_switch_links
            .has_changed()
            .expect("publication remains open")
    );
}

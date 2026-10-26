//! Which devices re-apply their volatile settings, and how many confirming passes they get.

use super::*;
use std::collections::BTreeMap;

#[test]
fn direct_identity_enrichment_carries_the_spent_followup_budget() {
    let snapshots = [None, Some("keyboard"), None].map(|serial| AgentDevice {
        route: Some(DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        }),
        serial: serial.map(str::to_string),
        unit_id: [1, 2, 3, 4],
        ..dev("keyboard", DIRECT_DEVICE_INDEX, true)
    });
    for pair in snapshots.windows(2) {
        let previous = &pair[..1];
        let next = &pair[1..];
        let old_id = stable_id(&previous[0]);
        let next_id = stable_id(&next[0]);
        assert_ne!(old_id, next_id);
        let followup = BTreeMap::from([(old_id, 2)]);
        let (targets, retained) = plan_reapply(previous, next, &followup, None, false);
        assert!(targets.is_empty(), "identity enrichment is not an arrival");
        assert_eq!(retained, BTreeMap::from([(next_id.clone(), 2)]));
        let (targets, retained) = plan_reapply(previous, next, &followup, None, true);
        assert_eq!(targets, [0]);
        assert_eq!(retained, BTreeMap::from([(next_id, 1)]));
        let (targets, retained) = plan_reapply(previous, next, &followup, Some(1), true);
        assert_eq!(targets, [0]);
        assert_eq!(retained.get(&stable_id(&next[0])), Some(&1));
    }
}

#[test]
fn direct_and_routeless_enrichment_requires_matching_nonzero_units() {
    use crate::orchestrator::devices::same_device_lifecycle;

    for route in [
        Some(DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        }),
        None,
    ] {
        for (before_unit, after_unit, expected) in [
            ([1, 2, 3, 4], [1, 2, 3, 4], true),
            ([1, 2, 3, 4], [5, 6, 7, 8], false),
            ([0; 4], [1, 2, 3, 4], false),
            ([1, 2, 3, 4], [0; 4], false),
            ([0; 4], [0; 4], false),
        ] {
            let previous = AgentDevice {
                route: route.clone(),
                unit_id: before_unit,
                ..dev("keyboard", DIRECT_DEVICE_INDEX, true)
            };
            let next = AgentDevice {
                route: route.clone(),
                serial: Some("keyboard".into()),
                unit_id: after_unit,
                ..dev("keyboard", DIRECT_DEVICE_INDEX, true)
            };
            assert_eq!(same_device_lifecycle(&previous, &next), expected);
            assert_eq!(same_device_lifecycle(&next, &previous), expected);
            let previous = AgentDevice {
                serial: Some("another-keyboard".into()),
                ..previous
            };
            assert!(!same_device_lifecycle(&previous, &next));
        }
    }
}

#[test]
fn regression_reapply_budget_does_not_transfer_to_replacement_in_same_slot() {
    let previous = [AgentDevice {
        unit_id: [1, 2, 3, 4],
        ..dev("legacy-slot", 1, true)
    }];
    let replacement = [AgentDevice {
        unit_id: [5, 6, 7, 8],
        ..dev("legacy-slot", 1, true)
    }];
    let (_, mut followup) = plan_reapply(&[], &previous, &BTreeMap::new(), None, false);
    (_, followup) = plan_reapply(&previous, &previous, &followup, None, true);
    assert_eq!(followup.get(&stable_id(&previous[0])), Some(&3));
    let (targets, followup) = plan_reapply(&previous, &replacement, &followup, None, false);
    assert_eq!(
        targets,
        [0],
        "replacement must receive its own first-sighting apply"
    );
    assert_eq!(
        followup.get(&stable_id(&replacement[0])),
        Some(&VOLATILE_REAPPLY_CONFIRM_RETRIES)
    );
}

#[test]
fn reapply_budget_survives_receiver_identity_enrichment() {
    let unknown = dev("legacy-slot", 1, true);
    let unit = AgentDevice {
        unit_id: [1, 2, 3, 4],
        ..dev("legacy-slot", 1, true)
    };
    let serial = AgentDevice {
        serial: Some("KEYBOARD".into()),
        unit_id: [1, 2, 3, 4],
        ..dev("legacy-slot", 1, true)
    };
    let normalized_serial = AgentDevice {
        serial: Some("keyboard".into()),
        unit_id: [1, 2, 3, 4],
        ..dev("serial:keyboard", 1, true)
    };
    for pair in [unknown, unit, serial, normalized_serial].windows(2) {
        let previous = &pair[..1];
        let next = &pair[1..];
        let followup = BTreeMap::from([(stable_id(&previous[0]), 2)]);
        let (targets, retained) = plan_reapply(previous, next, &followup, None, false);
        assert!(
            targets.is_empty(),
            "identity enrichment is not a new arrival"
        );
        assert_eq!(
            retained, followup,
            "ordinary snapshots preserve the spent budget"
        );
        let (targets, retained) = plan_reapply(previous, next, &followup, None, true);
        assert_eq!(targets, [0]);
        assert_eq!(retained.get(&stable_id(&next[0])), Some(&1));
    }
}

#[test]
fn reapply_budget_restarts_when_a_serial_only_receiver_device_is_replaced() {
    let previous = [AgentDevice {
        serial: Some("keyboard-a".into()),
        ..dev("legacy-slot", 1, true)
    }];
    let next = [AgentDevice {
        serial: Some("keyboard-b".into()),
        ..dev("legacy-slot", 1, true)
    }];
    let followup = BTreeMap::from([(stable_id(&previous[0]), 1)]);
    let (targets, followup) = plan_reapply(&previous, &next, &followup, None, true);
    assert_eq!(targets, [0]);
    assert_eq!(
        followup.get(&stable_id(&next[0])),
        Some(&VOLATILE_REAPPLY_CONFIRM_RETRIES)
    );
}

fn receiver_inventory(unit_id: [u8; 4]) -> DeviceInventory {
    let mut inventory = direct_inventory(None, unit_id);
    inventory.receiver.product_id = 0xc548;
    inventory.receiver.unique_id = Some("AA00".into());
    inventory.paired[0].slot = 1;
    inventory.paired[0].kind = DeviceKind::Keyboard;
    inventory
}

#[test]
fn receiver_replacement_and_removal_retire_scoped_recovery_and_late_requests() {
    use openlogi_hid::DisableKeysMask;

    for replacement in [None, Some(receiver_inventory([5, 6, 7, 8]))] {
        let mut orchestrator = orchestrator(Config::default());
        let previous = [receiver_inventory([1, 2, 3, 4])];
        orchestrator.refresh_inventory(&previous, &[], false);
        let shared = orchestrator.shared();
        let route = orchestrator.devices[0]
            .route
            .clone()
            .expect("receiver route");
        drop(
            shared
                .disabled_keys_order
                .begin(&route, Some(DisableKeysMask::EMPTY)),
        );
        orchestrator.refresh_inventory(&previous, &[], false);
        assert_eq!(orchestrator.disabled_keys_recovery.len(), 1);
        let pending = shared
            .disabled_keys_order
            .begin(&route, Some(DisableKeysMask::CAPS_LOCK));
        let ticket = pending.ticket();
        let next: Vec<_> = replacement.into_iter().collect();
        orchestrator.refresh_inventory(&next, &[], false);
        assert!(
            !ticket.is_current(),
            "old request cannot write to the replacement"
        );
        assert!(orchestrator.disabled_keys_recovery.is_empty());
        drop(pending);
        orchestrator.refresh_inventory(&next, &[], false);
        assert!(
            orchestrator.disabled_keys_recovery.is_empty(),
            "late cancellation cannot revive retired recovery"
        );
        if next.is_empty() {
            assert!(orchestrator.reapply_followup.is_empty());
            orchestrator.refresh_inventory(&previous, &[], false);
        }
        assert_eq!(
            orchestrator
                .reapply_followup
                .get(&stable_id(&orchestrator.devices[0])),
            Some(&VOLATILE_REAPPLY_CONFIRM_RETRIES)
        );
    }
}

#[test]
fn receiver_identity_enrichment_preserves_requests_and_scoped_recovery() {
    use openlogi_hid::DisableKeysMask;

    let mut orchestrator = orchestrator(Config::default());
    let mut inventory = receiver_inventory([0; 4]);
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    let shared = orchestrator.shared();
    let route = orchestrator.devices[0]
        .route
        .clone()
        .expect("receiver route");
    let pending = shared
        .disabled_keys_order
        .begin(&route, Some(DisableKeysMask::EMPTY));
    let ticket = pending.ticket();
    inventory.paired[0]
        .model_info
        .as_mut()
        .expect("model")
        .unit_id = [1, 2, 3, 4];
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(
        ticket.is_current(),
        "learning the unit ID must not retire a request"
    );
    drop(pending);
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );
    let recovery = orchestrator.disabled_keys_recovery.clone();
    assert_eq!(
        recovery.get(&stable_id(&orchestrator.devices[0])),
        Some(&(VOLATILE_REAPPLY_CONFIRM_RETRIES - 1))
    );
    inventory.paired[0]
        .model_info
        .as_mut()
        .expect("model")
        .serial_number = Some("keyboard".into());
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert_eq!(
        orchestrator.disabled_keys_recovery, recovery,
        "learning a serial keeps the remaining scoped budget"
    );
}

#[test]
fn reapply_targets_new_arrivals_and_transitions() {
    // First sighting of an online device → re-apply.
    assert_eq!(reapply_targets(&[], &[dev("a", 1, true)], false), vec![0]);
    // Steady state → nothing.
    assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, true)], false).is_empty());
    // Replug under a new route (same key, new slot) → re-apply.
    assert_eq!(
        reapply_targets(&[dev("a", 1, true)], &[dev("a", 2, true)], false),
        vec![0]
    );
    // Waking from device sleep (offline → online) → re-apply.
    assert_eq!(
        reapply_targets(&[dev("a", 1, false)], &[dev("a", 1, true)], false),
        vec![0]
    );
    // Going to sleep (online → offline) → nothing.
    assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, false)], false).is_empty());
}

#[test]
fn reapply_targets_disambiguates_same_model_duplicates() {
    // Two devices can share a model key but are distinct physical units at
    // different Bolt slots, so they have distinct stable ids. A steady tick
    // with both already online must target NEITHER.
    let prev = [dev("dup", 1, true), dev("dup", 2, true)];
    let next = [dev("dup", 1, true), dev("dup", 2, true)];
    assert!(reapply_targets(&prev, &next, false).is_empty());
}

#[test]
fn reapply_targets_skip_offline_and_routeless_devices() {
    // A paired-but-asleep new arrival waits for its online transition —
    // writing now would only time out against a sleeping device.
    assert!(reapply_targets(&[], &[dev("a", 1, false)], false).is_empty());
    let routeless = AgentDevice {
        route: None,
        ..dev("b", 2, true)
    };
    assert!(reapply_targets(&[], &[routeless], false).is_empty());
}

#[test]
fn reapply_all_targets_every_online_device() {
    let prev = [dev("a", 1, true), dev("b", 2, false)];
    let next = [dev("a", 1, true), dev("b", 2, false)];
    // The post-wake snapshot looks identical to the pre-sleep one; the
    // flag still re-applies to the online device (and only that one).
    assert_eq!(reapply_targets(&prev, &next, true), vec![0]);
}

#[test]
fn receiver_reconnect_requests_capture_rearm() {
    let prev = [dev("selected", 1, false), dev("other", 2, true)];
    let next = [dev("selected", 1, true), dev("other", 2, true)];

    assert_eq!(reapply_targets(&prev, &next, false), [0]);
    assert!(reapply_targets(&[dev("other", 2, true)], &[dev("other", 2, true)], false).is_empty());
}

#[test]
fn system_wake_requests_capture_rearm_for_online_devices() {
    let devices = [dev("selected", 1, true), dev("other", 2, true)];

    assert_eq!(reapply_targets(&devices, &devices, true), [0, 1]);
}

#[test]
fn steady_inventory_does_not_cycle_capture() {
    let devices = [dev("selected", 1, true)];

    assert!(reapply_targets(&devices, &devices, false).is_empty());
}

#[test]
fn plan_reapply_retries_a_first_sighting_for_a_bounded_run() {
    // First sighting: applied now, queued for VOLATILE_REAPPLY_CONFIRM_RETRIES
    // confirming re-applies. A cold restart can leave the device still
    // booting, so the initial write and a single confirm need a retry run,
    // not a one-shot confirm.
    let (targets, followup) = plan_reapply(&[], &[dev("a", 1, true)], &BTreeMap::new(), None, true);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES
        )])
    );
    // Each delayed confirmation after a first sighting re-applies once and
    // decrements the remaining retry budget — the device may still be booting.
    let prev = [dev("a", 1, true)];
    let followup_in = BTreeMap::from([(
        stable_id(&dev("a", 1, true)),
        VOLATILE_REAPPLY_CONFIRM_RETRIES,
    )]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, None, true);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES - 1,
        )])
    );
    // The budget exhausts: a last retry fires but queues no further ones.
    let followup_in = BTreeMap::from([(stable_id(&dev("a", 1, true)), 1)]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, None, true);
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
    // Steady state after that: nothing.
    let (targets, _) = plan_reapply(&prev, &prev, &BTreeMap::new(), None, true);
    assert!(targets.is_empty());
}

#[test]
fn plan_reapply_transitions_queue_one_link_race_confirm() {
    // A wake from device sleep re-applies now and queues exactly one
    // confirming pass: the device was already booted (no boot ladder), but
    // the immediate write races the link still re-establishing, and drowsy
    // firmware can ACK it yet keep the old value.
    let (targets, followup) = plan_reapply(
        &[dev("a", 1, false)],
        &[dev("a", 1, true)],
        &BTreeMap::new(),
        None,
        true,
    );
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            RECONNECT_REAPPLY_CONFIRM_RETRIES,
        )])
    );
    // The confirm drains on the next steady tick.
    let steady = [dev("a", 1, true)];
    let (targets, followup) = plan_reapply(&steady, &steady, &followup, None, true);
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
}

#[test]
fn plan_reapply_reconnect_mid_boot_run_keeps_the_larger_budget() {
    // A reconnect while boot confirmations are still draining must not shrink
    // the remaining boot ladder down to the link-race budget.
    let followup_in = BTreeMap::from([(
        stable_id(&dev("a", 1, true)),
        VOLATILE_REAPPLY_CONFIRM_RETRIES,
    )]);
    let (targets, followup) = plan_reapply(
        &[dev("a", 1, false)],
        &[dev("a", 1, true)],
        &followup_in,
        None,
        true,
    );
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES - 1,
        )])
    );
}

#[test]
fn plan_reapply_wake_targets_get_a_confirm_retry_run() {
    // A system wake re-applies to every online device *and* queues the same
    // confirm-retry run a first sighting gets: post-wake, a receiver can
    // enumerate while its mouse link is still re-establishing, so the first
    // write can time out just like the cold-boot race (#527). Offline devices
    // stay untargeted and unqueued; they re-apply on their own transition.
    let prev = [dev("a", 1, true), dev("b", 2, false)];
    let (targets, followup) = plan_reapply(
        &prev,
        &prev,
        &BTreeMap::new(),
        Some(VOLATILE_REAPPLY_CONFIRM_RETRIES),
        false,
    );
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES
        )])
    );
    // The run then drains at the usual cadence on steady ticks.
    let (targets, followup) = plan_reapply(&prev, &prev, &followup, None, true);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES - 1,
        )])
    );
}

#[test]
fn plan_reapply_device_wake_forces_the_link_race_budget() {
    // A `0x1d4b` reconnect broadcast forces a re-apply even on a snapshot
    // with no observable transition (the nap was too short for any
    // reconciliation to have seen the device offline), but queues only the
    // single link-race confirm — not the boot ladder — so an ordinary mouse
    // nap costs two write rounds, not five.
    let prev = [dev("a", 1, true)];
    let (targets, followup) = plan_reapply(
        &prev,
        &prev,
        &BTreeMap::new(),
        Some(RECONNECT_REAPPLY_CONFIRM_RETRIES),
        false,
    );
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            RECONNECT_REAPPLY_CONFIRM_RETRIES,
        )])
    );
    let (targets, followup) = plan_reapply(&prev, &prev, &followup, None, true);
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
}

#[test]
fn plan_reapply_skips_a_followup_that_went_offline() {
    let prev = [dev("a", 1, true)];
    let (targets, followup) = plan_reapply(
        &prev,
        &[dev("a", 1, false)],
        &BTreeMap::from([(
            stable_id(&dev("a", 1, true)),
            VOLATILE_REAPPLY_CONFIRM_RETRIES,
        )]),
        None,
        true,
    );
    assert!(targets.is_empty());
    assert!(followup.is_empty());
}

#[test]
fn confirmation_budget_survives_config_key_adoption() {
    use openlogi_core::device_order::PhysicalDeviceKey;

    let legacy = "receiver:aa00:slot:1";
    let mut config = Config::default();
    config.set_disabled_keys(legacy, BTreeSet::from([DisableKey::CapsLock]));
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.product_id = 0xc548;
    inventory.receiver.unique_id = Some("AA00".into());
    inventory.paired[0].slot = 1;
    let mut orchestrator = orchestrator(config.clone());

    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert_eq!(orchestrator.devices[0].config_key, legacy);
    assert!(orchestrator.needs_reapply_confirmation());

    let canonical = PhysicalDeviceKey::parse("unit:01020304").expect("key");
    assert!(config.adopt_route(&canonical, legacy, None));
    orchestrator.reload_config(config);
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );

    assert_eq!(orchestrator.devices[0].config_key, canonical.as_str());
    assert_eq!(
        orchestrator
            .reapply_followup
            .get(&stable_id(&orchestrator.devices[0])),
        Some(&(VOLATILE_REAPPLY_CONFIRM_RETRIES - 1)),
        "the active physical device keeps its confirmation budget after key adoption"
    );
}

#[test]
fn orchestrator_exposes_only_the_bounded_confirmation_run() {
    let mut orchestrator = orchestrator(Config::default());
    let inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);

    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(orchestrator.needs_reapply_confirmation());

    for _ in 0..(VOLATILE_REAPPLY_CONFIRM_RETRIES + 2) {
        orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
        assert!(
            orchestrator.needs_reapply_confirmation(),
            "ordinary lifecycle snapshots must preserve the confirmation budget"
        );
    }

    for confirmations_left in (0..VOLATILE_REAPPLY_CONFIRM_RETRIES).rev() {
        orchestrator.refresh_inventory_for_settings_confirmation(
            std::slice::from_ref(&inventory),
            &[],
            false,
        );
        assert_eq!(
            orchestrator.needs_reapply_confirmation(),
            confirmations_left > 0
        );
    }
}

#[test]
fn reconnect_broadcast_forces_one_confirming_reapply() {
    let mut orchestrator = orchestrator(Config::default());
    let inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
        orchestrator.refresh_inventory_for_settings_confirmation(
            std::slice::from_ref(&inventory),
            &[],
            false,
        );
    }
    assert!(!orchestrator.needs_reapply_confirmation());
    // A `0x1d4b` reconnect broadcast: the next snapshot re-applies (despite no
    // observable transition) and queues exactly one confirming pass.
    orchestrator.reapply_reconnected_on_next_refresh();
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(orchestrator.needs_reapply_confirmation());
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );
    assert!(!orchestrator.needs_reapply_confirmation());
}

#[test]
fn forced_reapply_requests_coalesce_to_the_larger_budget() {
    let mut orchestrator = orchestrator(Config::default());
    let inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
        orchestrator.refresh_inventory_for_settings_confirmation(
            std::slice::from_ref(&inventory),
            &[],
            false,
        );
    }
    // A reconnect broadcast landing after a system wake (before its snapshot)
    // must not shrink the wake's boot-race ladder.
    orchestrator.reapply_volatile_on_next_refresh();
    orchestrator.reapply_reconnected_on_next_refresh();
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    for confirmations_left in (0..VOLATILE_REAPPLY_CONFIRM_RETRIES).rev() {
        assert!(orchestrator.needs_reapply_confirmation());
        orchestrator.refresh_inventory_for_settings_confirmation(
            std::slice::from_ref(&inventory),
            &[],
            false,
        );
        assert_eq!(
            orchestrator.needs_reapply_confirmation(),
            confirmations_left > 0
        );
    }
}

/// A mouse whose probe found `0x8100` onboard profiles.
fn onboard_mouse(key: &str, slot: u8, online: bool) -> AgentDevice {
    let mut device = dev(key, slot, online);
    device.capabilities = Some(Capabilities {
        onboard_profiles: true,
        ..Capabilities::default()
    });
    device
}

fn onboard_mouse_config(onboard_profiles: Option<bool>) -> Config {
    let mut config = Config::default();
    config.set_dpi("a", Dpi::new(3200));
    if let Some(onboard_profiles) = onboard_profiles {
        config.set_onboard_profiles("a", onboard_profiles);
    }
    config
}

#[test]
fn a_host_control_choice_switches_before_restoring_the_users_dpi() {
    let config = onboard_mouse_config(Some(false));

    assert_eq!(
        onboard_switch(&config, &onboard_mouse("a", 1, true)),
        Some((false, Some(Dpi::new(3200))))
    );
    // The DPI rides the ordered onboard write, not the unordered reapply.
    assert_eq!(reconnect_dpi(&config, &onboard_mouse("a", 1, true)), None);
}

#[test]
fn an_active_onboard_profile_gets_no_host_dpi() {
    let config = onboard_mouse_config(Some(true));

    assert_eq!(
        onboard_switch(&config, &onboard_mouse("a", 1, true)),
        Some((true, None))
    );
    assert_eq!(reconnect_dpi(&config, &onboard_mouse("a", 1, true)), None);
}

#[test]
fn without_an_onboard_choice_the_mode_is_left_and_the_dpi_reapplied() {
    let config = onboard_mouse_config(None);

    assert_eq!(onboard_switch(&config, &onboard_mouse("a", 1, true)), None);
    assert_eq!(
        reconnect_dpi(&config, &onboard_mouse("a", 1, true)),
        Some(Dpi::new(3200))
    );
}

#[test]
fn a_device_without_onboard_profiles_gets_no_switch_and_keeps_its_dpi() {
    let config = onboard_mouse_config(Some(false));
    let mut device = onboard_mouse("a", 1, true);
    device.capabilities = Some(Capabilities::default());

    assert_eq!(onboard_switch(&config, &device), None);
    assert_eq!(reconnect_dpi(&config, &device), Some(Dpi::new(3200)));
    device.capabilities = None;
    assert_eq!(onboard_switch(&config, &device), None);
    assert_eq!(reconnect_dpi(&config, &device), Some(Dpi::new(3200)));
}

#[test]
fn a_reload_switches_onboard_mode_only_on_an_online_enabled_device() {
    let devices = [
        onboard_mouse("unchanged", 1, true),
        onboard_mouse("changed", 2, true),
        onboard_mouse("disabled", 3, true),
        onboard_mouse("offline", 4, false),
        onboard_mouse("removed", 5, true),
    ];
    let mut previous = Config::default();
    previous.set_onboard_profiles("unchanged", false);
    previous.set_onboard_profiles("changed", true);
    previous.set_onboard_profiles("removed", false);
    let mut config = Config::default();
    config.set_onboard_profiles("unchanged", false);
    config.set_onboard_profiles("changed", false);
    config.set_dpi("changed", Dpi::new(1600));
    config.set_onboard_profiles("disabled", false);
    config.set_device_enabled("disabled", false);
    config.set_onboard_profiles("offline", false);

    assert_eq!(
        changed_onboard_switches(&previous, &config, &devices),
        vec![(
            devices[1].route.clone().expect("routed"),
            false,
            Some(Dpi::new(1600))
        )]
    );
}

#[test]
fn every_switch_to_host_control_restores_the_users_dpi() {
    // Also a retry after a refused switch, when the saved choice is already
    // host control and no config reload would write the DPI.
    let config = onboard_mouse_config(Some(false));

    assert_eq!(
        host_dpi(&config, &dev("a", 1, true), false),
        Some(Dpi::new(3200))
    );
    assert_eq!(host_dpi(&config, &dev("a", 1, true), true), None);
}

#[test]
fn a_hidpp20_slot_reconnect_targets_only_online_devices_behind_such_receivers() {
    let behind_hidpp20 = |slot, online| AgentDevice {
        route: Some(DeviceRoute::Hidpp20Receiver {
            receiver_uid: "695A8298".to_string(),
            slot,
        }),
        ..dev("x3", slot, online)
    };
    let devices = [
        dev("bolt-mouse", 1, true),
        behind_hidpp20(1, true),
        behind_hidpp20(2, false),
    ];

    assert_eq!(hidpp20_slot_targets(&devices), vec![1]);
}

#[test]
fn a_hidpp20_slot_reconnect_never_narrows_a_pending_wake_reapply() {
    let mut orchestrator = orchestrator(Config::default());
    orchestrator.reapply_hidpp20_slots_on_next_refresh();
    assert_eq!(orchestrator.forced_reapply, ForcedReapply::Hidpp20Slots);

    orchestrator.reapply_volatile_on_next_refresh();
    orchestrator.reapply_hidpp20_slots_on_next_refresh();
    assert_eq!(orchestrator.forced_reapply, ForcedReapply::All);
}

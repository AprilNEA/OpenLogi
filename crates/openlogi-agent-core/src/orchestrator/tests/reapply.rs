//! Which devices re-apply their volatile settings, and how many confirming passes they get.

use super::*;

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

    assert!(any_device_needs_capture_rearm(&prev, &next, false));
    assert!(!any_device_needs_capture_rearm(
        &[dev("other", 2, true)],
        &[dev("other", 2, true)],
        false
    ));
}

#[test]
fn system_wake_requests_capture_rearm_for_online_devices() {
    let devices = [dev("selected", 1, true), dev("other", 2, true)];

    assert!(any_device_needs_capture_rearm(&devices, &devices, true));
}

#[test]
fn steady_inventory_does_not_cycle_capture() {
    let devices = [dev("selected", 1, true)];

    assert!(!any_device_needs_capture_rearm(&devices, &devices, false));
}

#[test]
fn plan_reapply_retries_a_first_sighting_for_a_bounded_run() {
    use std::collections::HashMap;
    // First sighting: applied now, queued for VOLATILE_REAPPLY_CONFIRM_RETRIES
    // confirming re-applies. A cold restart can leave the device still
    // booting, so the initial write and a single confirm need a retry run,
    // not a one-shot confirm.
    let (targets, followup) = plan_reapply(&[], &[dev("a", 1, true)], &HashMap::new(), false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)])
    );
    // Each steady tick after a first sighting re-applies once and decrements
    // the remaining retry budget — the device may still be booting.
    let prev = [dev("a", 1, true)];
    let followup_in = HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES - 1)])
    );
    // The budget exhausts: a last retry fires but queues no further ones.
    let followup_in = HashMap::from([("a".to_string(), 1)]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, false);
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
    // Steady state after that: nothing.
    let (targets, _) = plan_reapply(&prev, &prev, &HashMap::new(), false);
    assert!(targets.is_empty());
}

#[test]
fn plan_reapply_transitions_are_not_queued_for_confirmation() {
    use std::collections::HashMap;
    // A wake from device sleep re-applies once — the device was already
    // booted, so no confirming write is queued.
    let (targets, followup) = plan_reapply(
        &[dev("a", 1, false)],
        &[dev("a", 1, true)],
        &HashMap::new(),
        false,
    );
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
}

#[test]
fn plan_reapply_wake_targets_get_a_confirm_retry_run() {
    use std::collections::HashMap;
    // A system wake re-applies to every online device *and* queues the same
    // confirm-retry run a first sighting gets: post-wake, a receiver can
    // enumerate while its mouse link is still re-establishing, so the first
    // write can time out just like the cold-boot race (#527). Offline devices
    // stay untargeted and unqueued; they re-apply on their own transition.
    let prev = [dev("a", 1, true), dev("b", 2, false)];
    let (targets, followup) = plan_reapply(&prev, &prev, &HashMap::new(), true);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)])
    );
    // The run then drains at the usual cadence on steady ticks.
    let (targets, followup) = plan_reapply(&prev, &prev, &followup, false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES - 1)])
    );
}

#[test]
fn plan_reapply_skips_a_followup_that_went_offline() {
    use std::collections::HashMap;
    let prev = [dev("a", 1, true)];
    let (targets, followup) = plan_reapply(
        &prev,
        &[dev("a", 1, false)],
        &HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)]),
        false,
    );
    assert!(targets.is_empty());
    assert!(followup.is_empty());
}

#[test]
fn orchestrator_exposes_only_the_bounded_confirmation_run() {
    let mut orchestrator = orchestrator(Config::default());
    let inventory = direct_inventory(Some("serial-1"), [1, 2, 3, 4]);

    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(orchestrator.needs_reapply_confirmation());

    for confirmations_left in (0..VOLATILE_REAPPLY_CONFIRM_RETRIES).rev() {
        orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
        assert_eq!(
            orchestrator.needs_reapply_confirmation(),
            confirmations_left > 0
        );
    }
}

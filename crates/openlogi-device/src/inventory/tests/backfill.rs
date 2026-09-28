//! Backfilling an incomplete probe from the cache, field by field.

use super::*;

/// A control-table read that fails half way reads exactly like "no haptic
/// panel", and the answer is memoized for `REFRESH_INTERVAL` — so the Actions Ring
/// binding would vanish from the GUI for half a minute on a device that has it.
#[test]
fn an_incomplete_capability_walk_keeps_the_last_complete_answer() {
    let mut fresh = probed(None, false);
    fresh.capabilities_incomplete = true;
    fresh.capabilities = Some(Capabilities::default());
    let mut cached = probed(None, false);
    cached.capabilities = Some(Capabilities {
        haptic_panel: true,
        dpi_gestures: true,
        ..Capabilities::default()
    });

    keep_known_capabilities(&mut fresh, &cached);

    assert_eq!(
        fresh.capabilities, cached.capabilities,
        "the last complete control walk must survive a lost reply"
    );
    assert!(
        fresh.capabilities_incomplete,
        "the failed probe still needs repair"
    );
}

/// A device that genuinely lost a capability must still be able to say so.
#[test]
fn a_complete_capability_walk_is_left_alone() {
    let mut fresh = probed(None, false);
    fresh.capabilities = Some(Capabilities::default());
    let mut cached = probed(None, false);
    cached.capabilities = Some(Capabilities {
        haptic_panel: true,
        dpi_gestures: true,
        ..Capabilities::default()
    });

    keep_known_capabilities(&mut fresh, &cached);

    assert_eq!(fresh.capabilities, Some(Capabilities::default()));
}

#[test]
fn failed_device_info_read_backfills_from_cache() {
    let mut fresh = probed(None, true);
    let cached = probed(Some(model([0x46, 0, 0x2e, 0], None)), false);

    backfill_identity(&mut fresh, &cached);

    assert_eq!(fresh.model_info, cached.model_info);
    assert!(
        !fresh.identity_incomplete,
        "a backfilled identity is complete and may be cached"
    );
}

#[test]
fn failed_serial_read_backfills_only_the_serial() {
    let mut fresh = probed(Some(model([1, 2, 3, 4], None)), true);
    let cached = probed(Some(model([9, 9, 9, 9], Some("abc123"))), false);

    backfill_identity(&mut fresh, &cached);

    let Some(info) = fresh.model_info else {
        panic!("model info kept");
    };
    assert_eq!(info.serial_number.as_deref(), Some("abc123"));
    assert_eq!(info.unit_id, [1, 2, 3, 4], "fresh unit id wins");
    assert!(!fresh.identity_incomplete);
}

#[test]
fn complete_probe_is_never_overwritten_by_cache() {
    let mut fresh = probed(Some(model([1, 2, 3, 4], None)), false);
    let cached = probed(Some(model([9, 9, 9, 9], Some("stale"))), false);

    backfill_identity(&mut fresh, &cached);

    let Some(info) = fresh.model_info else {
        panic!("model info kept");
    };
    assert_eq!(info.unit_id, [1, 2, 3, 4]);
    assert!(
        info.serial_number.is_none(),
        "no serial was read, none faked"
    );
}

#[test]
fn incomplete_probe_without_cached_identity_stays_incomplete() {
    let mut fresh = probed(None, true);
    let cached = probed(None, false);

    backfill_identity(&mut fresh, &cached);

    assert!(
        fresh.identity_incomplete,
        "nothing to backfill from — the caller must not memoize this probe"
    );
}

#[test]
fn failed_kind_read_is_carried_forward() {
    let mut fresh = ProbedFeatures::default();
    let cached = probed(None, false);

    backfill_identity(&mut fresh, &cached);

    assert_eq!(fresh.kind, Some(DeviceKind::Mouse));
}

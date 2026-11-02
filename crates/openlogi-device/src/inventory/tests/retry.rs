//! The one-shot retry after an incomplete first enumeration.

use super::*;

#[test]
fn one_shot_retry_stops_when_first_attempt_is_complete() {
    let current = inventory(&[1, 2]);
    let scan = OneShotScan::new();

    assert!(
        scan.is_settled(
            &current,
            ScanPass {
                complete: true,
                healthy: true
            }
        ),
        "complete inventories keep the one-pass happy path"
    );
}

#[test]
fn one_shot_retry_waits_for_healthy_incomplete_inventory_to_stabilize() {
    let partial = inventory(&[1]);
    let full = inventory(&[1, 2]);
    let healthy = ScanPass {
        complete: false,
        healthy: true,
    };
    let mut scan = OneShotScan::new();

    assert!(
        !scan.is_settled(&partial, healthy),
        "the first incomplete pass has no previous inventory to compare"
    );
    scan.advance(partial, healthy);
    assert!(
        !scan.is_settled(&full, healthy),
        "a changed inventory should get another retry window"
    );
    scan.advance(full.clone(), healthy);
    assert!(
        scan.is_settled(&full, healthy),
        "once the returned inventory stabilizes, retrying stops"
    );
}

#[test]
fn one_shot_retry_stops_on_unchanged_incomplete_inventory() {
    let partial = inventory(&[1]);
    let healthy = ScanPass {
        complete: false,
        healthy: true,
    };
    let mut scan = OneShotScan::new();

    scan.advance(partial.clone(), healthy);
    assert!(
        scan.is_settled(&partial, healthy),
        "stable partial inventories should not burn every retry attempt"
    );
}

#[test]
fn one_shot_retry_keeps_unchanged_inventory_after_unhealthy_probe() {
    let partial = inventory(&[1]);
    let mut scan = OneShotScan::new();

    // The replayed snapshot arrived from an earlier healthy pass…
    scan.advance(
        partial.clone(),
        ScanPass {
            complete: false,
            healthy: true,
        },
    );
    // …but this pass failed, so the unchanged replay is not stability
    // evidence.
    assert!(
        !scan.is_settled(
            &partial,
            ScanPass {
                complete: false,
                healthy: false
            }
        ),
        "unchanged replay after a failed probe must keep retrying before the cap"
    );
}

#[test]
fn one_shot_retry_stops_at_attempt_cap_when_inventory_keeps_changing() {
    let unhealthy = ScanPass {
        complete: false,
        healthy: false,
    };
    let mut scan = OneShotScan::new();

    while scan.attempt < ONESHOT_ATTEMPTS {
        let changing = inventory(&[scan.attempt]);
        assert!(
            !scan.is_settled(&changing, unhealthy),
            "attempts below the cap keep retrying"
        );
        scan.advance(changing, unhealthy);
    }
    assert!(
        scan.is_settled(&inventory(&[1, 2]), unhealthy),
        "the retry loop must remain bounded even if the inventory changes every time"
    );
}

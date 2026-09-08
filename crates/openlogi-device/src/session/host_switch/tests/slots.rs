use super::*;

#[tokio::test]
async fn switching_to_an_unpaired_slot_is_refused() {
    // ChangeHost would allow it: host 2 is within the device's channel
    // count. But nothing is paired there, and `setCurrentHost` is
    // fire-and-forget - the device would simply leave and not come back.
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;

    let Err(error) = prepare_host_change_on(&channel, 1, 2).await else {
        panic!("an unpaired slot must not be switched to");
    };

    assert!(
        matches!(error, HostSwitchError::HostSlotEmpty { host: 2 }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn switching_to_a_paired_slot_proceeds() {
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;

    let change = prepare_host_change_on(&channel, 1, 1)
        .await
        .expect("a paired slot must be switchable");

    assert!(change.required, "host 1 differs from the current host 0");
}

#[tokio::test]
async fn a_device_without_hosts_info_is_still_switched() {
    // 0x1815 is the only source of per-slot pairing status. Without it the
    // guard must not block, or this change would regress every device that
    // does not implement it.
    let channel = scripted_channel(keyboard_without_hosts_info).await;

    let change = prepare_host_change_on(&channel, 1, 2)
        .await
        .expect("a device that cannot report slot status must still switch");

    assert!(change.required);
}

#[tokio::test]
async fn a_failed_hosts_info_lookup_does_not_block_the_switch() {
    // Firmware that answers an unknown feature id with an error rather than
    // index 0 must read the same as not implementing 0x1815 at all: the
    // pairing status is unknowable, which is not a reason to refuse.
    let channel = scripted_channel(keyboard_erroring_on_hosts_info_lookup).await;

    let change = prepare_host_change_on(&channel, 1, 2)
        .await
        .expect("an errored feature lookup must not abort the switch");

    assert!(change.required);
}

#[tokio::test]
async fn an_unreadable_slot_status_does_not_block_the_switch() {
    // The guard is advisory. A device that has 0x1815 but cannot answer for
    // it right now has not said the slot is empty, so refusing here would
    // turn a transient read failure into a dead host key.
    let channel = scripted_channel(keyboard_erroring_on_slot_status).await;

    let change = prepare_host_change_on(&channel, 1, 2)
        .await
        .expect("an errored status read must not abort the switch");

    assert!(change.required);
}

#[tokio::test]
async fn a_switch_to_the_current_host_never_consults_slot_status() {
    // Already-there is decided before the pairing check, so a device on an
    // unpaired-looking slot is not blocked from staying put.
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;

    let change = prepare_host_change_on(&channel, 1, 0)
        .await
        .expect("staying on the current host is always fine");

    assert!(!change.required);
}

#[test]
fn current_host_does_not_require_a_change() {
    assert!(matches!(host_change_required(1, 3, 1), Ok(false)));
}

#[test]
fn different_valid_host_requires_a_change() {
    assert!(matches!(host_change_required(0, 3, 2), Ok(true)));
}

#[test]
fn host_outside_device_range_is_rejected() {
    assert!(
        host_change_required(0, 2, 2).is_err(),
        "host 2 is outside a device that reports 2 hosts and must be rejected"
    );
}

#[test]
fn only_departure_errors_mark_a_device_unreachable() {
    assert!(HostSwitchError::KeyboardNotFound.is_device_unreachable());
    assert!(HostSwitchError::Hid(BackendError::Disconnected).is_device_unreachable());
    assert!(HostSwitchError::Device(DeviceError::DeviceNotFound).is_device_unreachable());
    assert!(
        !HostSwitchError::Hidpp("DeviceNotFound".into()).is_device_unreachable(),
        "formatted debug text must not classify a departure"
    );

    assert!(
        !HostSwitchError::HostSlotEmpty { host: 2 }.is_device_unreachable(),
        "an unpaired slot must never be classified as a departure"
    );
    assert!(
        !HostSwitchError::TimedOut {
            operation: "probing"
        }
        .is_device_unreachable()
    );
    assert!(
        !HostSwitchError::Hid(BackendError::Backend("permission denied".into()))
            .is_device_unreachable()
    );
}

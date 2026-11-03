use std::sync::{Arc, OnceLock};

use hidpp::channel::HidppChannel;

use hidpp::feature::change_host::HostChangeRequest;
use tokio::sync::{mpsc, oneshot};

use super::{
    ArmedControl, HostSwitchError, HostSwitchRestoreOutcome, HostSwitchSessionOutcome,
    HostSwitchStop, HostTable, HostTrigger, KeyboardHosts, KeyboardPosition,
    PendingHostSwitchRestore, ReportingMode, announced_host, host_change_required, host_channel,
    key_trigger, monitor_host_switch, prepare_host_change_on, prepare_host_switch_on,
    queued_departure, restoration_change, rollback_host_switch_start, run_host_switch_session,
    shares_channel, switch_host_on,
};
use crate::ChannelPool;
use crate::backend::NodeId;
use crate::channel::scripted::{
    ScriptedBackend, ScriptedRawHidChannel, feature_error, scripted_channel as raw_scripted_channel,
};
use crate::replay::ReplayChannelHandle;
use crate::reprog_controls::{
    AnalyticsKeyEvent, CidReporting, ControlId, CtrlIdInfo, ReprogControlsEvent,
};
use crate::{ChannelRegistry, DeviceRoute, SharedChannel, device_io_channel};

/// Feature index the scripted keyboard reports for `0x1814 ChangeHost`.
const CHANGE_HOST_INDEX: u8 = 0x04;
/// Feature index the scripted keyboard reports for `0x1815 HostsInfo`.
const HOSTS_INFO_INDEX: u8 = 0x05;

/// `ErrorType::Busy`, the failure a scripted device answers with.
const BUSY: u8 = 0x08;

/// `ErrorType::HwError`, the failure of a keyboard that is leaving.
const HW_ERROR: u8 = 0x04;

/// What the scripted keyboard's firmware does when asked about `0x1815`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotStatus {
    /// Answers the query: hosts 0 and 1 paired, host 2 empty.
    Reported,
    /// Reports the feature as unimplemented, the usual index-0 lookup miss.
    Unimplemented,
    /// Errors on the lookup itself, as firmware that refuses unknown
    /// feature ids rather than reporting index 0 does.
    LookupErrors,
    /// Implements the feature but errors on the status read.
    ReadErrors,
    /// Implements the feature but never answers the status read.
    ReadIgnored,
    /// Reports `status == Empty` for every slot, including paired ones, but
    /// still reports a nonzero friendly-name length for them — the MX
    /// Vertical quirk from #1709.
    StuckEmptyButNamed,
}

/// A three-channel keyboard currently on host 0, paired on hosts 0 and 1
/// but **not** on host 2 — a keyboard with three host keys and only two
/// machines paired, which is the shape that used to strand devices.
fn keyboard_with_an_empty_third_slot(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::Reported)
}

/// The same keyboard without `0x1815`, so its slot pairing is unknowable.
fn keyboard_without_hosts_info(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::Unimplemented)
}

/// The same keyboard, whose firmware errors when asked for `0x1815`.
fn keyboard_erroring_on_hosts_info_lookup(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::LookupErrors)
}

/// The same keyboard, whose `0x1815` reads come back an error.
fn keyboard_erroring_on_slot_status(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::ReadErrors)
}

/// A keyboard whose `0x1815` status byte is stuck at `Empty` for every slot
/// but still reports a real friendly-name length — paired slots must still
/// be switchable.
fn keyboard_with_stuck_empty_status(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::StuckEmptyButNamed)
}

fn scripted_keyboard(request: &[u8], slot_status: SlotStatus) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let mut payload = [0u8; 16];
    match (request[2], request[3] >> 4) {
        // Root ping used by Device::new.
        (0x00, 0x01) => payload[0] = 4,
        // Root feature lookup.
        (0x00, 0x00) => {
            payload[0] = match u16::from_be_bytes([request[4], request[5]]) {
                0x1814 => CHANGE_HOST_INDEX,
                0x1815 => match slot_status {
                    SlotStatus::Unimplemented => 0x00,
                    SlotStatus::LookupErrors => return Some(feature_error(request, BUSY)),
                    SlotStatus::Reported
                    | SlotStatus::ReadErrors
                    | SlotStatus::ReadIgnored
                    | SlotStatus::StuckEmptyButNamed => HOSTS_INFO_INDEX,
                },
                _ => 0x00,
            };
        }
        // ChangeHost getHostInfo: three RF channels, currently on host 0.
        (CHANGE_HOST_INDEX, 0x00) => payload[..2].copy_from_slice(&[3, 0]),
        // HostsInfo getHostInfo: echo the slot, then its pairing status.
        (HOSTS_INFO_INDEX, 0x01) => {
            match slot_status {
                SlotStatus::ReadErrors => return Some(feature_error(request, BUSY)),
                SlotStatus::ReadIgnored => return None,
                _ => {}
            }
            payload[0] = request[4];
            if slot_status == SlotStatus::StuckEmptyButNamed {
                payload[1] = 0; // status stuck at Empty on every slot
                payload[4] = 5; // but a real friendly-name length
            } else {
                payload[1] = u8::from(request[4] < 2);
            }
        }
        _ => return None,
    }

    // Short reports carry only 3 payload bytes; `name_len` lives at index 4,
    // so answering with it requires a long (0x11) report.
    if payload[3..].iter().any(|&byte| byte != 0) {
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        return Some(response);
    }
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload[..3]);
    Some(response)
}

async fn scripted_channel(responder: crate::channel::scripted::Responder) -> Arc<HidppChannel> {
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(responder);
    crate::channel::scripted::scripted_channel(raw).await
}

#[tokio::test]
async fn switching_to_an_unpaired_slot_is_refused() {
    // ChangeHost would allow it: host 2 is within the device's channel
    // count. But nothing is paired there, and `setCurrentHost` is
    // fire-and-forget — the device would simply leave and not come back.
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
async fn a_stuck_empty_status_does_not_override_a_nonzero_name_length() {
    // #1709: the device's status byte is stuck at Empty for every slot, but
    // a nonzero friendly-name length is independent evidence the slot is
    // really paired, so the switch must still be allowed.
    let channel = scripted_channel(keyboard_with_stuck_empty_status).await;

    let change = prepare_host_change_on(&channel, 1, 2)
        .await
        .expect("a nonzero name length must override a stuck Empty status");

    assert!(change.required, "host 2 differs from the current host 0");
}

#[tokio::test]
async fn switching_to_a_paired_slot_proceeds() {
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;

    let change = prepare_host_change_on(&channel, 1, 1)
        .await
        .expect("a paired slot must be switchable");

    assert!(change.required, "host 1 differs from the current host 0");
}

/// A shared channel for one Bolt device, plus the handle that records writes.
async fn shared_device(
    responder: crate::channel::scripted::Responder,
) -> (SharedChannel, ReplayChannelHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(responder);
    let channel = raw_scripted_channel(raw).await;
    let route = DeviceRoute::Bolt {
        receiver_uid: "AABB".into(),
        slot: 1,
    };
    (SharedChannel::new(channel, route), handle)
}

/// Whether a `ChangeHost setCurrentHost(host)` report was written.
fn wrote_set_current_host(handle: &ReplayChannelHandle, host: u8) -> bool {
    handle.written_reports().iter().any(|report| {
        report.len() >= 5
            && report[2] == CHANGE_HOST_INDEX
            && report[3] >> 4 == 1
            && report[4] == host
    })
}

#[tokio::test]
async fn switching_one_device_issues_the_fire_and_forget_write() {
    let (shared, handle) = shared_device(keyboard_with_an_empty_third_slot).await;

    let changed = switch_host_on(&shared, 1)
        .await
        .expect("a paired slot must be switchable");

    assert!(changed);
    assert!(wrote_set_current_host(&handle, 1));
}

#[tokio::test]
async fn switching_one_device_honours_a_name_over_a_stuck_empty_status() {
    // #1709 on the single-device path: Flow must still reach an MX Vertical
    // whose status byte reads Empty for every slot.
    let (shared, handle) = shared_device(keyboard_with_stuck_empty_status).await;

    let changed = switch_host_on(&shared, 2)
        .await
        .expect("a named slot must be switchable despite a stuck Empty status");

    assert!(changed);
    assert!(wrote_set_current_host(&handle, 2));
}

#[tokio::test]
async fn switching_one_device_to_an_unpaired_slot_writes_nothing() {
    let (shared, handle) = shared_device(keyboard_with_an_empty_third_slot).await;

    let error = switch_host_on(&shared, 2)
        .await
        .expect_err("an unpaired slot must be refused");

    assert!(
        matches!(error, HostSwitchError::HostSlotEmpty { host: 2 }),
        "got {error:?}"
    );
    assert!(!wrote_set_current_host(&handle, 2));
}

#[tokio::test]
async fn a_prepared_switch_writes_only_when_applied() {
    // Flow prepares every device before it writes to any of them, so the
    // preparation must not move this one.
    let (shared, handle) = shared_device(keyboard_with_an_empty_third_slot).await;

    let prepared = prepare_host_switch_on(&shared, 1)
        .await
        .expect("a paired slot must be switchable");

    assert_eq!(prepared.host(), 1);
    assert!(prepared.is_required());
    assert!(!wrote_set_current_host(&handle, 1));
    assert!(prepared.apply().await.expect("the write is acknowledged"));
    assert!(wrote_set_current_host(&handle, 1));
}

#[tokio::test]
async fn a_prepared_switch_to_the_current_host_writes_nothing() {
    let (shared, handle) = shared_device(keyboard_with_an_empty_third_slot).await;

    let prepared = prepare_host_switch_on(&shared, 0)
        .await
        .expect("staying on the current host is always fine");

    assert!(!prepared.is_required());
    assert!(!prepared.apply().await.expect("nothing to write"));
    assert!(!wrote_set_current_host(&handle, 0));
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
async fn an_unanswered_slot_status_does_not_use_up_the_switch_budget() {
    // A device that answers ChangeHost but ignores the HostsInfo read must
    // still be switched within Flow's 1 s per-device preparation, and long
    // before the agent's 5 s budget for the whole switch_host.
    let channel =
        scripted_channel(|request| scripted_keyboard(request, SlotStatus::ReadIgnored)).await;
    let started = tokio::time::Instant::now();

    let change = prepare_host_change_on(&channel, 1, 2)
        .await
        .expect("an unanswered status read must not abort the switch");

    assert!(change.required);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the advisory read took {:?}",
        started.elapsed()
    );
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

/// A reporting snapshot with unrelated bits deliberately set, so the
/// cleanup tests prove that only the bits they vary are restored.
fn noisy_reporting() -> CidReporting {
    CidReporting {
        cid: ControlId(0x00d3),
        diverted: false,
        persistently_diverted: true,
        force_raw_xy: true,
        raw_xy: false,
        remap: Some(ControlId(0x1234)),
        analytics_key_events: false,
        raw_wheel: true,
    }
}

fn direct_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    }
}

fn armed_control() -> ArmedControl {
    ArmedControl {
        cid: 0x00d3,
        host: 0,
        mode: ReportingMode::Diverted,
        original: noisy_reporting(),
    }
}

#[tokio::test]
async fn pending_restore_recovers_only_through_a_new_current_publication() {
    let route = direct_route();
    let node = NodeId::from("keyboard-node".to_owned());
    let registry = ChannelRegistry::default();
    let (retired_raw, retired_handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = raw_scripted_channel(retired_raw).await;
    let retired = SharedChannel::new(retired_channel.clone(), route.clone());
    registry.replace_node(node.clone(), [route.clone()], retired_channel);
    let pending = PendingHostSwitchRestore::new(&retired, 0x22, vec![armed_control()])
        .expect("one armed control must require restoration");

    let pending = match pending.retry(&registry).await {
        HostSwitchRestoreOutcome::RestorePending(pending) => pending,
        HostSwitchRestoreOutcome::Restored => {
            panic!("the retired publication must not restore itself")
        }
    };
    assert!(retired_handle.written_reports().is_empty());

    let (failed_raw, failed_handle) =
        ScriptedRawHidChannel::with_failing_writes(|request| Some(request.to_vec()), |_| true);
    let failed = raw_scripted_channel(failed_raw).await;
    registry.replace_node(node.clone(), [route.clone()], failed);
    let pending = match pending.retry(&registry).await {
        HostSwitchRestoreOutcome::RestorePending(pending) => pending,
        HostSwitchRestoreOutcome::Restored => panic!("failed writes cannot restore firmware"),
    };
    assert_eq!(failed_handle.written_reports().len(), 2);

    let (fresh_raw, fresh_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(node, [route], raw_scripted_channel(fresh_raw).await);

    assert!(matches!(
        pending.retry(&registry).await,
        HostSwitchRestoreOutcome::Restored
    ));
    assert_eq!(fresh_handle.written_reports().len(), 1);
}

#[tokio::test]
async fn suspended_partial_arm_rollback_returns_pending_without_writes() {
    let route = direct_route();
    let node = NodeId::from("keyboard-node".to_owned());
    let registry = ChannelRegistry::default();
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let channel = raw_scripted_channel(raw).await;
    let shared = SharedChannel::new(channel.clone(), route.clone());
    registry.replace_node(node, [route], channel);
    let pending = PendingHostSwitchRestore::new(&shared, 0x22, vec![armed_control()]);
    let (signal, gate) = device_io_channel();
    assert!(signal.suspend());

    let failure = rollback_host_switch_start(
        HostSwitchError::Hidpp("partial arm failed".into()),
        pending,
        &registry,
        &gate,
    )
    .await;
    let (_, pending) = failure.into_parts();

    assert!(handle.written_reports().is_empty());
    assert!(signal.resume());
    assert!(matches!(
        pending
            .expect("failed rollback must retain ownership")
            .retry(&registry)
            .await,
        HostSwitchRestoreOutcome::Restored
    ));
    assert_eq!(handle.written_reports().len(), 1);
}

#[test]
fn receiver_slots_share_one_channel() {
    let keyboard = DeviceRoute::Bolt {
        receiver_uid: "AABB".into(),
        slot: 1,
    };
    let mouse = DeviceRoute::Bolt {
        receiver_uid: "aabb".into(),
        slot: 2,
    };
    assert!(shares_channel(&keyboard, &mouse));
}

#[test]
fn direct_devices_do_not_share_channels() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb025,
    };
    assert!(!shares_channel(&route, &route));
}

#[test]
fn host_controls_are_recognized_by_task_when_cid_varies() {
    let info = CtrlIdInfo {
        cid: 0x1234,
        task_id: 0x00af,
        flags: 0,
    };
    assert_eq!(host_channel(info), Some(1));
}

#[test]
fn analytics_event_selects_the_matching_host() {
    let controls = [ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    }];
    let mut events = [AnalyticsKeyEvent::default(); 5];
    events[0] = AnalyticsKeyEvent {
        cid: ControlId(0x00d3),
        event: 1,
    };
    assert_eq!(
        key_trigger(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events)),
        Some(HostTrigger::Reported(2))
    );
}

#[test]
fn a_key_event_counts_only_for_the_mode_its_control_was_armed_in() {
    let control = |mode| ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode,
        original: noisy_reporting(),
    };
    let mut analytics = [AnalyticsKeyEvent::default(); 5];
    analytics[0] = AnalyticsKeyEvent {
        cid: ControlId(0x00d3),
        event: 1,
    };
    let diverted = ReprogControlsEvent::DivertedButtons([
        ControlId(0x00d3),
        ControlId(0),
        ControlId(0),
        ControlId(0),
    ]);
    assert_eq!(
        key_trigger(&[control(ReportingMode::Diverted)], diverted),
        Some(HostTrigger::Pressed(2))
    );
    // A diverted key does not switch the keyboard, so its analytics report
    // must not move the mice ahead of it.
    assert_eq!(
        key_trigger(
            &[control(ReportingMode::Diverted)],
            ReprogControlsEvent::AnalyticsKeyEvents(analytics)
        ),
        None
    );
    assert_eq!(
        key_trigger(&[control(ReportingMode::Analytics)], diverted),
        None
    );
}

#[test]
fn only_a_switch_away_from_this_host_is_a_departure() {
    let position = KeyboardPosition {
        feature_index: CHANGE_HOST_INDEX,
        version: 2,
        host_count: 3,
        current_host: 1,
        announces: true,
    };
    let announced =
        |from, to| position.stop_for(HostTrigger::Announced(HostChangeRequest { from, to }));
    assert_eq!(announced(1, 0), Some(HostSwitchStop::Departed(0)));
    assert_eq!(announced(1, 2), Some(HostSwitchStop::Departed(2)));
    assert_eq!(
        announced(0, 1),
        None,
        "the reversed byte order would move the mice to this host"
    );
    assert_eq!(announced(1, 1), None, "no change");
    assert_eq!(announced(1, 3), None, "outside the host count");
    assert_eq!(
        position.stop_for(HostTrigger::Reported(2)),
        Some(HostSwitchStop::Departed(2))
    );
    assert_eq!(
        position.stop_for(HostTrigger::Reported(1)),
        None,
        "the key of the current host does not switch"
    );
    assert_eq!(position.stop_for(HostTrigger::Reported(3)), None);
    assert_eq!(
        position.stop_for(HostTrigger::Pressed(1)),
        Some(HostSwitchStop::Pressed(1)),
        "a diverted press goes to the commanded transition"
    );
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
    let error = host_change_required(0, 2, 2).expect_err("host 2 must be rejected");

    assert!(matches!(
        error,
        HostSwitchError::HostOutOfRange {
            host: 2,
            host_count: 2
        }
    ));
}

#[test]
fn host_switch_errors_cross_ipc_as_the_easy_switch_vocabulary() {
    use openlogi_core::hid::{HidppOperation, WriteError};

    let cases = [
        (
            HostSwitchError::FeatureUnsupported,
            WriteError::FeatureUnsupported {
                feature_hex: 0x1814,
            },
        ),
        (
            HostSwitchError::HostOutOfRange {
                host: 3,
                host_count: 2,
            },
            WriteError::HostOutOfRange {
                slot: 3,
                host_count: 2,
            },
        ),
        (
            HostSwitchError::HostSlotEmpty { host: 2 },
            WriteError::HostSlotEmpty { slot: 2 },
        ),
        (
            HostSwitchError::TimedOut {
                operation: "reading current host",
            },
            WriteError::RequestTimedOut {
                operation: HidppOperation::ChangeHost,
            },
        ),
        (HostSwitchError::TargetNotFound, WriteError::DeviceNotFound),
        (
            HostSwitchError::Hidpp("busy".into()),
            WriteError::Hidpp("busy".into()),
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(WriteError::from(error), expected);
    }
}

#[test]
fn diverted_cleanup_restores_only_the_original_temporary_bits() {
    let change = restoration_change(ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Diverted,
        original: CidReporting {
            diverted: true,
            raw_xy: true,
            ..noisy_reporting()
        },
    });

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(true));
    assert_eq!(change.analytics_key_events, None);
    assert_eq!(change.persistently_diverted, None);
    assert_eq!(change.remap, None);
}

#[test]
fn analytics_cleanup_restores_the_original_analytics_bit() {
    let change = restoration_change(ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: CidReporting {
            analytics_key_events: true,
            ..noisy_reporting()
        },
    });

    assert_eq!(change.analytics_key_events, Some(true));
    assert_eq!(change.diverted, None);
    assert_eq!(change.raw_xy, None);
}

/// Receiver slot of the scripted keyboard in [`bolt_pair`].
const KEYBOARD_SLOT: u8 = 1;
/// Receiver slot of the scripted mouse in [`bolt_pair`].
const MOUSE_SLOT: u8 = 2;
/// Feature index the scripted mouse reports for `0x1814 ChangeHost`.
const MOUSE_CHANGE_HOST_INDEX: u8 = 0x06;
/// The keyboard's `ReprogControlsV4` index in [`bolt_pair`].
const REPROG_INDEX: u8 = 0x07;

/// The keyboard of [`bolt_pair`].
#[derive(Clone, Copy)]
struct PairedKeyboard {
    /// The `ChangeHost` version that the keyboard reports.
    change_host_version: u8,
    /// The keyboard reports `ANNOUNCES_HOST_CHANGE`.
    announces: bool,
    /// The keyboard has `0x1815`, with hosts 0 and 1 paired and host 2 empty.
    hosts_info: bool,
    /// The keyboard has `0x1b04` with one control: the key for host 1, which
    /// reports analytics events but cannot be diverted.
    analytics_host_key: bool,
}

/// A Bolt receiver with a keyboard in slot 1 and a mouse in slot 2, both on
/// host 0 of three. The mouse has neither `0x1b04` nor `0x1815`.
fn bolt_pair(request: &[u8], keyboard: PairedKeyboard) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_id = u16::from_be_bytes([request[4], request[5]]);
    let mut payload = [0u8; 16];
    match (request[1], request[2], request[3] >> 4) {
        (KEYBOARD_SLOT | MOUSE_SLOT, 0x00, 0x01) => payload[0] = 4,
        (KEYBOARD_SLOT, 0x00, 0x00) => match feature_id {
            0x1814 => {
                payload[0] = CHANGE_HOST_INDEX;
                payload[2] = keyboard.change_host_version;
            }
            0x1815 if keyboard.hosts_info => payload[0] = HOSTS_INFO_INDEX,
            0x1b04 if keyboard.analytics_host_key => payload[0] = REPROG_INDEX,
            _ => {}
        },
        (KEYBOARD_SLOT, CHANGE_HOST_INDEX, 0x00) | (MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 0x00) => {
            payload[..2].copy_from_slice(&[3, 0]);
        }
        (KEYBOARD_SLOT, CHANGE_HOST_INDEX, 0x04) => payload[0] = u8::from(keyboard.announces),
        (KEYBOARD_SLOT, HOSTS_INFO_INDEX, 0x00) => payload[2] = 3,
        (KEYBOARD_SLOT, HOSTS_INFO_INDEX, 0x01) => {
            payload[0] = request[4];
            payload[1] = u8::from(request[4] < 2);
        }
        (KEYBOARD_SLOT, REPROG_INDEX, 0x00) => payload[0] = 1,
        (KEYBOARD_SLOT, REPROG_INDEX, 0x01) => {
            payload[..4].copy_from_slice(&[0x00, HOST_1_CID, 0x00, 0xaf]);
            // Additional flags: analytics key events only.
            payload[8] = 0x04;
        }
        (KEYBOARD_SLOT, REPROG_INDEX, 0x02) => payload[..2].copy_from_slice(&request[4..6]),
        (KEYBOARD_SLOT, REPROG_INDEX, 0x03) => {
            let echo = (request.len() - 4).min(payload.len());
            payload[..echo].copy_from_slice(&request[4..4 + echo]);
        }
        (MOUSE_SLOT, 0x00, 0x00) if feature_id == 0x1814 => payload[0] = MOUSE_CHANGE_HOST_INDEX,
        (MOUSE_SLOT, 0x00, 0x00) => {}
        _ => return None,
    }
    // A control's information does not fit a short report.
    if request[2] == REPROG_INDEX {
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        return Some(response);
    }
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload[..3]);
    Some(response)
}

/// The control ID (low byte) of the keyboard's key for host 1 in
/// [`bolt_pair`].
const HOST_1_CID: u8 = 0xd2;

/// The keyboard's `0x1b04` analytics event for the key for host 1.
fn analytics_report() -> Vec<u8> {
    let mut report = vec![0u8; 20];
    report[..7].copy_from_slice(&[
        0x11,
        KEYBOARD_SLOT,
        REPROG_INDEX,
        0x20,
        0x00,
        HOST_1_CID,
        0x01,
    ]);
    report
}

fn bolt_route(slot: u8) -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: "cafe".to_owned(),
        slot,
    }
}

/// The keyboard's `ChangeHost` event 0: it leaves `from` for `to`.
fn announcement(from: u8, to: u8) -> Vec<u8> {
    vec![0x10, KEYBOARD_SLOT, CHANGE_HOST_INDEX, 0x00, from, to, 0]
}

fn is_request(report: &[u8], slot: u8, feature_index: u8, function: u8) -> bool {
    report[1] == slot && report[2] == feature_index && report[3] >> 4 == function
}

/// Run a session for the keyboard of [`bolt_pair`] with the mouse linked,
/// send `announcement` once the session listens, and return the outcome
/// and every report written.
async fn announce_to_session(
    keyboard: PairedKeyboard,
    announcement: Vec<u8>,
) -> (
    Result<HostSwitchSessionOutcome, super::HostSwitchSessionFailure>,
    Vec<Vec<u8>>,
) {
    // The capability read comes after the listener is installed.
    trigger_session(keyboard, (CHANGE_HOST_INDEX, 4), announcement).await
}

/// Run a session for the keyboard of [`bolt_pair`] with the mouse linked.
/// Send `report` from the keyboard once the session has written function
/// `ready.1` of feature index `ready.0` to it, and return the outcome and
/// every report written.
async fn trigger_session(
    keyboard: PairedKeyboard,
    ready: (u8, u8),
    report: Vec<u8>,
) -> (SessionResult, Vec<Vec<u8>>) {
    let session = RunningSession::start(move |request| bolt_pair(request, keyboard)).await;
    let (ready_index, ready_function) = ready;
    session.written(ready_index, ready_function, 1).await;
    session.send(report);
    session.outcome().await
}

type SessionResult = Result<HostSwitchSessionOutcome, super::HostSwitchSessionFailure>;

/// A session for the keyboard in [`KEYBOARD_SLOT`] with the mouse in
/// [`MOUSE_SLOT`] linked, on a channel that a responder answers.
struct RunningSession {
    task: tokio::task::JoinHandle<SessionResult>,
    incoming: mpsc::UnboundedSender<Vec<u8>>,
    handle: crate::replay::ReplayChannelHandle,
}

impl RunningSession {
    /// Spawn the session. It runs once the test yields.
    async fn start(responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(responder);
        let incoming = raw.incoming_sender();
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("bolt-receiver".to_owned()),
            [bolt_route(KEYBOARD_SLOT), bolt_route(MOUSE_SLOT)],
            raw_scripted_channel(raw).await,
        );
        let task = tokio::spawn(async move {
            let (_stop, stopped) = oneshot::channel();
            let (_signal, gate) = device_io_channel();
            let pool = ChannelPool::with_backend(ScriptedBackend::new(Vec::new()));
            run_host_switch_session(
                bolt_route(KEYBOARD_SLOT),
                &[bolt_route(MOUSE_SLOT)],
                stopped,
                &registry,
                &pool,
                gate,
            )
            .await
        });
        Self {
            task,
            incoming,
            handle,
        }
    }

    /// Wait until the session has written `count` requests for function
    /// `function` of feature index `feature_index` to the keyboard.
    async fn written(&self, feature_index: u8, function: u8, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while self
                .handle
                .written_reports()
                .iter()
                .filter(|written| is_request(written, KEYBOARD_SLOT, feature_index, function))
                .count()
                < count
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("the session writes {count} of function {function} of feature {feature_index}")
        });
    }

    /// Send `report` from the keyboard.
    fn send(&self, report: Vec<u8>) {
        self.incoming.send(report).expect("the channel is open");
    }

    /// The outcome of the session, and every report written.
    async fn outcome(self) -> (SessionResult, Vec<Vec<u8>>) {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), self.task)
            .await
            .expect("the session ends")
            .expect("the session task does not panic");
        (outcome, self.handle.written_reports())
    }
}

/// The keyboard's reply to `setHostFriendlyName` (`0x1815` function 4).
fn rename_reply() -> Vec<u8> {
    let mut report = vec![0u8; 20];
    report[..5].copy_from_slice(&[0x11, KEYBOARD_SLOT, HOSTS_INFO_INDEX, 0x41, 0x01]);
    report
}

#[tokio::test]
async fn a_departure_announced_while_the_host_keys_arm_still_moves_the_mouse() {
    // The keyboard announces its switch while the session arms its host
    // keys. Then it leaves, and the arm fails (#1623).
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: false,
        hosts_info: false,
        analytics_host_key: true,
    };
    let incoming = Arc::new(OnceLock::<mpsc::UnboundedSender<Vec<u8>>>::new());
    let announce = Arc::clone(&incoming);
    let session = RunningSession::start(move |request| {
        if is_request(request, KEYBOARD_SLOT, REPROG_INDEX, 3) {
            if let Some(incoming) = announce.get() {
                let _ = incoming.send(announcement(0, 1));
            }
            return Some(feature_error(request, HW_ERROR));
        }
        bolt_pair(request, keyboard)
    })
    .await;
    incoming
        .set(session.incoming.clone())
        .expect("the sender is set once");
    let (outcome, writes) = session.outcome().await;

    assert!(
        matches!(
            outcome,
            Ok(HostSwitchSessionOutcome::Departed { restore: Some(_) })
        ),
        "the failed write is restored after the keyboard is back"
    );
    let failed_at = writes
        .iter()
        .position(|report| is_request(report, KEYBOARD_SLOT, REPROG_INDEX, 3))
        .expect("the session enables analytics reporting");
    let after_failure = &writes[failed_at + 1..];
    assert!(
        after_failure
            .iter()
            .all(|report| report[1] != KEYBOARD_SLOT),
        "no rollback write waits for the keyboard that is leaving"
    );
    assert!(
        after_failure.iter().any(|report| {
            is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1) && report[4] == 1
        }),
        "the mouse follows to host 1"
    );
}

/// Start a session whose keyboard announces a switch from host 0 to host 1
/// while the session reads its current host. `host_read` answers that read.
async fn announce_during_host_read(
    host_read: impl Fn(&[u8], PairedKeyboard) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> (SessionResult, Vec<Vec<u8>>) {
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: true,
        hosts_info: false,
        analytics_host_key: false,
    };
    let incoming = Arc::new(OnceLock::<mpsc::UnboundedSender<Vec<u8>>>::new());
    let announce = Arc::clone(&incoming);
    let session = RunningSession::start(move |request| {
        if is_request(request, KEYBOARD_SLOT, CHANGE_HOST_INDEX, 0) {
            if let Some(incoming) = announce.get() {
                let _ = incoming.send(announcement(0, 1));
            }
            return host_read(request, keyboard);
        }
        bolt_pair(request, keyboard)
    })
    .await;
    incoming
        .set(session.incoming.clone())
        .expect("the sender is set once");
    session.outcome().await
}

fn moves_mouse_to_host_1(writes: &[Vec<u8>]) -> bool {
    writes
        .iter()
        .any(|report| is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1) && report[4] == 1)
}

#[tokio::test]
async fn an_announcement_while_the_session_reads_the_host_still_moves_the_mouse() {
    // The announcement comes before the reply to the host read.
    let (outcome, writes) = announce_during_host_read(bolt_pair).await;
    assert!(
        matches!(outcome, Ok(HostSwitchSessionOutcome::Departed { .. })),
        "the session hears the switch"
    );
    assert!(
        moves_mouse_to_host_1(&writes),
        "the mouse follows to host 1"
    );
}

#[tokio::test]
async fn an_announcement_before_a_failed_host_read_still_moves_the_mouse() {
    // The keyboard leaves before it answers the host read.
    let (outcome, writes) =
        announce_during_host_read(|request, _| Some(feature_error(request, HW_ERROR))).await;
    assert!(
        matches!(
            outcome,
            Ok(HostSwitchSessionOutcome::Departed { restore: None })
        ),
        "nothing was armed, so nothing is restored"
    );
    let failed_at = writes
        .iter()
        .position(|report| is_request(report, KEYBOARD_SLOT, CHANGE_HOST_INDEX, 0))
        .expect("the session reads the keyboard's host");
    let after_failure = &writes[failed_at + 1..];
    assert!(
        after_failure
            .iter()
            .all(|report| report[1] != KEYBOARD_SLOT),
        "nothing more is written to the keyboard that is leaving"
    );
    assert!(
        moves_mouse_to_host_1(after_failure),
        "the mouse follows to host 1"
    );
}

#[tokio::test]
async fn a_rename_makes_the_session_read_the_keyboard_slots_again() {
    // A departure maps the slots by name. After a rename through this agent,
    // the names that the session read when it armed are old.
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: true,
        hosts_info: true,
        analytics_host_key: false,
    };
    let session = RunningSession::start(move |request| bolt_pair(request, keyboard)).await;
    session.written(HOSTS_INFO_INDEX, 1, 3).await;
    session.send(rename_reply());
    session.written(HOSTS_INFO_INDEX, 1, 6).await;
    session.send(announcement(0, 1));
    let (outcome, writes) = session.outcome().await;

    assert!(matches!(
        outcome,
        Ok(HostSwitchSessionOutcome::Departed { restore: None })
    ));
    assert_eq!(
        writes
            .iter()
            .filter(|report| is_request(report, KEYBOARD_SLOT, HOSTS_INFO_INDEX, 1))
            .count(),
        6,
        "the three slots are read when the session arms and once after the rename"
    );
    assert!(
        writes.iter().any(|report| {
            is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1) && report[4] == 1
        }),
        "the mouse follows to host 1"
    );
}

#[tokio::test]
async fn an_announced_switch_moves_the_mouse_and_leaves_the_keyboard_alone() {
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: true,
        hosts_info: false,
        analytics_host_key: false,
    };
    let (outcome, writes) = announce_to_session(keyboard, announcement(0, 1)).await;

    assert!(
        matches!(
            outcome,
            Ok(HostSwitchSessionOutcome::Departed { restore: None })
        ),
        "an announcing keyboard has no host keys to restore"
    );
    let mouse_moves: Vec<_> = writes
        .iter()
        .filter(|report| is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1))
        .collect();
    assert_eq!(mouse_moves.len(), 1, "the mouse follows once");
    assert_eq!(mouse_moves[0][4], 1, "to the keyboard's new host");
    assert!(
        !writes
            .iter()
            .any(|report| is_request(report, KEYBOARD_SLOT, CHANGE_HOST_INDEX, 1)),
        "the keyboard moves by itself"
    );
    assert!(
        writes
            .iter()
            .filter(|report| report[1] == KEYBOARD_SLOT)
            .all(|report| matches!(report[2], 0x00 | CHANGE_HOST_INDEX)),
        "the keyboard's host keys keep their reporting"
    );
}

#[tokio::test]
async fn a_switch_to_an_empty_keyboard_slot_leaves_the_mouse() {
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: true,
        hosts_info: true,
        analytics_host_key: false,
    };
    let (outcome, writes) = announce_to_session(keyboard, announcement(0, 2)).await;

    assert!(matches!(
        outcome,
        Ok(HostSwitchSessionOutcome::Departed { restore: None })
    ));
    assert!(
        !writes
            .iter()
            .any(|report| is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1)),
        "slot 2 of the mouse belongs to another computer"
    );
}

#[tokio::test]
async fn a_keyboard_without_the_capability_or_host_keys_is_still_heard() {
    // A K880 has no host keys that report to software, but announces a
    // switch before it leaves (#1145).
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: false,
        hosts_info: false,
        analytics_host_key: false,
    };
    let (outcome, writes) = announce_to_session(keyboard, announcement(0, 1)).await;

    assert!(matches!(
        outcome,
        Ok(HostSwitchSessionOutcome::Departed { restore: None })
    ));
    assert!(
        writes
            .iter()
            .any(|report| is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1)),
        "the mouse follows the announcement"
    );
}

#[tokio::test]
async fn an_analytics_report_moves_the_mouse_without_writing_to_the_keyboard() {
    // The keyboard reports the key and leaves at once. A request to it after
    // the report waits for a keyboard that is gone (#1623).
    let keyboard = PairedKeyboard {
        change_host_version: 2,
        announces: false,
        hosts_info: false,
        analytics_host_key: true,
    };
    let (outcome, writes) = trigger_session(keyboard, (REPROG_INDEX, 3), analytics_report()).await;

    assert!(
        matches!(
            outcome,
            Ok(HostSwitchSessionOutcome::Departed { restore: Some(_) })
        ),
        "the analytics bit is restored after the keyboard is back"
    );
    let armed_at = writes
        .iter()
        .position(|report| is_request(report, KEYBOARD_SLOT, REPROG_INDEX, 3))
        .expect("the session enables analytics reporting");
    let after_report = &writes[armed_at + 1..];
    assert!(
        after_report.iter().all(|report| report[1] != KEYBOARD_SLOT),
        "no request goes to the keyboard after it reports the key"
    );
    assert!(
        after_report.iter().any(|report| {
            is_request(report, MOUSE_SLOT, MOUSE_CHANGE_HOST_INDEX, 1) && report[4] == 1
        }),
        "the mouse follows to host 1"
    );
}

#[test]
fn a_departure_queued_before_the_channel_changed_still_moves_the_linked_devices() {
    let position = KeyboardPosition {
        feature_index: 3,
        version: 2,
        host_count: 3,
        current_host: 0,
        announces: false,
    };
    let (triggers_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
    triggers_tx.send(HostTrigger::Pressed(2)).unwrap();
    triggers_tx.send(HostTrigger::Reported(0)).unwrap();
    triggers_tx.send(HostTrigger::Reported(1)).unwrap();
    assert_eq!(
        queued_departure(&mut triggers, position, &mut KeyboardHosts::default()),
        Some(HostSwitchStop::Departed(1)),
        "a press and a report for this host are skipped"
    );

    triggers_tx.send(HostTrigger::Pressed(1)).unwrap();
    assert_eq!(
        queued_departure(&mut triggers, position, &mut KeyboardHosts::default()),
        None,
        "a press needs the keyboard's channel"
    );
}

#[tokio::test]
async fn an_offline_receiver_slot_keeps_a_queued_departure_but_drops_a_press() {
    let route = DeviceRoute::Bolt {
        receiver_uid: "receiver".to_owned(),
        slot: KEYBOARD_SLOT,
    };
    let registry = ChannelRegistry::default();
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let channel = raw_scripted_channel(raw).await;
    let shared = SharedChannel::new(Arc::clone(&channel), route.clone());
    let node = NodeId::from("receiver-node".to_owned());
    registry.replace_node(node.clone(), [route], Arc::clone(&channel));
    registry.replace_node(node, [], channel);
    let position = KeyboardPosition {
        feature_index: 3,
        version: 2,
        host_count: 3,
        current_host: 0,
        announces: false,
    };
    for (trigger, expected) in [
        (HostTrigger::Reported(1), HostSwitchStop::Departed(1)),
        (HostTrigger::Pressed(1), HostSwitchStop::ReceiverSlotGone),
    ] {
        let (_shutdown_tx, shutdown) = oneshot::channel();
        let (triggers_tx, mut triggers) = mpsc::unbounded_channel();
        triggers_tx.send(trigger).unwrap();
        let (_signal, gate) = device_io_channel();
        let stop = monitor_host_switch(
            shutdown,
            &mut triggers,
            position,
            &registry,
            &shared,
            gate,
            &mut KeyboardHosts::default(),
        )
        .await;
        assert_eq!(stop, expected, "queued trigger: {trigger:?}");
    }
    assert!(handle.written_reports().is_empty());
}

#[test]
fn a_failed_host_read_follows_only_an_announcement_to_another_host() {
    let (triggers_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(announced_host(&mut triggers), None);
    triggers_tx.send(HostTrigger::Renamed).unwrap();
    triggers_tx.send(HostTrigger::Reported(2)).unwrap();
    triggers_tx
        .send(HostTrigger::Announced(HostChangeRequest { from: 1, to: 1 }))
        .unwrap();
    triggers_tx
        .send(HostTrigger::Announced(HostChangeRequest { from: 0, to: 2 }))
        .unwrap();
    assert_eq!(
        announced_host(&mut triggers),
        Some(2),
        "a report and a change to the same host are skipped"
    );
}

#[tokio::test]
async fn an_offline_receiver_departure_restores_when_its_slot_returns_on_the_same_channel() {
    let route = DeviceRoute::Bolt {
        receiver_uid: "receiver".to_owned(),
        slot: KEYBOARD_SLOT,
    };
    let registry = ChannelRegistry::default();
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let channel = raw_scripted_channel(raw).await;
    let shared = SharedChannel::new(Arc::clone(&channel), route.clone());
    let node = NodeId::from("receiver-node".to_owned());
    registry.replace_node(node.clone(), [route.clone()], Arc::clone(&channel));
    let pending = PendingHostSwitchRestore::new(&shared, 0x22, vec![armed_control()])
        .expect("armed host control requires restoration");
    registry.replace_node(node.clone(), [], Arc::clone(&channel));
    let (_signal, gate) = device_io_channel();
    let pool = ChannelPool::with_backend(ScriptedBackend::new(Vec::new()));
    let outcome = super::depart(
        super::Departure::new(&shared, None, 1),
        Some(pending),
        super::Followers {
            targets: &[],
            registry: &registry,
            channel_pool: &pool,
            device_io: &gate,
        },
    )
    .await;
    let (follow_up, pending) = outcome.into_parts();
    assert_eq!(follow_up, super::HostSwitchFollowUp::AwaitDeparture);
    let pending = match pending.unwrap().retry(&registry).await {
        HostSwitchRestoreOutcome::RestorePending(pending) => pending,
        HostSwitchRestoreOutcome::Restored => panic!("an offline slot cannot restore"),
    };
    assert!(handle.written_reports().is_empty());
    registry.replace_node(node, [route], channel);
    assert!(matches!(
        pending.retry(&registry).await,
        HostSwitchRestoreOutcome::Restored
    ));
    assert_eq!(handle.written_reports().len(), 1);
}

#[test]
fn a_rename_queued_before_a_departure_drops_the_old_slot_names() {
    // The keyboard is leaving, so the session cannot read the names again.
    let position = KeyboardPosition {
        feature_index: 3,
        version: 2,
        host_count: 3,
        current_host: 0,
        announces: true,
    };
    let (triggers_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
    triggers_tx.send(HostTrigger::Renamed).unwrap();
    triggers_tx
        .send(HostTrigger::Announced(HostChangeRequest { from: 0, to: 1 }))
        .unwrap();
    let mut hosts = KeyboardHosts::with_table(HostTable(Vec::new()));
    assert_eq!(
        queued_departure(&mut triggers, position, &mut hosts),
        Some(HostSwitchStop::Departed(1))
    );
    assert!(
        hosts.table().is_none(),
        "the linked devices keep their slot numbers"
    );
}

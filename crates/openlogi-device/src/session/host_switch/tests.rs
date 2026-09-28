use super::{HostSwitchRequest, KeyboardHostTransition, ReportedHostSlot, switch_linked_hosts};
use crate::ChannelPool;
use crate::channel::scripted::{ScriptedBackend, ScriptedOpen, scripted_node_info};
use crate::replay::ReplayChannelHandle as ScriptedRawHidHandle;
use std::sync::Arc;
mod transition;

use hidpp::channel::HidppChannel;

use super::{
    ArmedControl, HostSwitchError, HostSwitchRestoreOutcome, PendingHostSwitchRestore,
    ReportingMode, event_control, host_change_required, prepare_host_change_on, restoration_change,
    rollback_host_switch_start, shares_channel,
};
use crate::backend::NodeId;
use crate::channel::scripted::{
    ScriptedRawHidChannel, feature_error, scripted_channel as raw_scripted_channel,
};
use crate::reprog_controls::{
    AnalyticsKeyEvent, CidReporting, ControlId, CtrlIdInfo, ReprogControlsEvent,
    host_switch_channel as host_channel,
};
use crate::{ChannelRegistry, DeviceRoute, SharedChannel, device_io_channel};

/// Feature index the scripted keyboard reports for `0x1814 ChangeHost`.
const CHANGE_HOST_INDEX: u8 = 0x04;
/// Feature index the scripted keyboard reports for `0x1815 HostsInfo`.
const HOSTS_INFO_INDEX: u8 = 0x05;

/// `ErrorType::Busy`, the failure a scripted device answers with.
const BUSY: u8 = 0x08;

/// What the scripted keyboard's firmware does when asked about `0x1815`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotStatus {
    /// Answers the query: hosts 0 and 1 paired, host 2 empty.
    Reported,
    /// Answers the query with all three hosts paired.
    AllPaired,
    /// Reports the feature as unimplemented, the usual index-0 lookup miss.
    Unimplemented,
    /// Errors on the lookup itself, as firmware that refuses unknown
    /// feature ids rather than reporting index 0 does.
    LookupErrors,
    /// Implements the feature but errors on the status read.
    ReadErrors,
    /// Implements the feature but never answers the status read.
    ReadTimesOut,
    /// Implements the feature but returns an unknown pairing-status value.
    Unrecognized,
}

/// A three-channel keyboard currently on host 0, paired on hosts 0 and 1
/// but **not** on host 2 - a keyboard with three host keys and only two
/// machines paired, which is the shape that used to strand devices.
fn keyboard_with_an_empty_third_slot(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::Reported)
}

/// The same keyboard with every reported host slot paired.
fn keyboard_with_all_slots_paired(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::AllPaired)
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

/// The same keyboard, whose `0x1815` status reads never answer.
fn keyboard_timing_out_on_slot_status(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::ReadTimesOut)
}

/// The same keyboard, whose `0x1815` status value is not recognized.
fn keyboard_with_unrecognized_slot_status(request: &[u8]) -> Option<Vec<u8>> {
    scripted_keyboard(request, SlotStatus::Unrecognized)
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
                    | SlotStatus::AllPaired
                    | SlotStatus::ReadErrors
                    | SlotStatus::ReadTimesOut
                    | SlotStatus::Unrecognized => HOSTS_INFO_INDEX,
                },
                _ => 0x00,
            };
        }
        // ChangeHost getHostInfo: three RF channels, currently on host 0.
        (CHANGE_HOST_INDEX, 0x00) => payload[..2].copy_from_slice(&[3, 0]),
        // HostsInfo getHostInfo: echo the slot, then its pairing status.
        (HOSTS_INFO_INDEX, 0x01) => {
            if slot_status == SlotStatus::ReadTimesOut {
                return None;
            }
            if slot_status == SlotStatus::ReadErrors {
                return Some(feature_error(request, BUSY));
            }
            payload[0] = request[4];
            payload[1] = if slot_status == SlotStatus::Unrecognized {
                0xff
            } else {
                u8::from(slot_status == SlotStatus::AllPaired || request[4] < 2)
            };
        }
        _ => return None,
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

async fn scripted_live_node(
    id: &str,
    product_id: u16,
    responder: crate::channel::scripted::Responder,
) -> (crate::backend::NodeInfo, ScriptedOpen, ScriptedRawHidHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(responder);
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let mut node = scripted_node_info(id);
    node.product_id = product_id;
    (node, ScriptedOpen::Live(channel), handle)
}

fn sent_host_change(handle: &ScriptedRawHidHandle, host: u8) -> bool {
    handle.written_reports().iter().any(|report| {
        report.get(2) == Some(&CHANGE_HOST_INDEX)
            && report.get(3).is_some_and(|function| function >> 4 == 1)
            && report.get(4) == Some(&host)
    })
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
        event_control(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events))
            .map(|control| control.host),
        Some(2)
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
    assert!(
        host_change_required(0, 2, 2).is_err(),
        "host 2 is outside a device that reports 2 hosts and must be rejected"
    );
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

fn analytics_request(host: u8) -> HostSwitchRequest {
    HostSwitchRequest {
        host,
        keyboard_transition: KeyboardHostTransition::AnalyticsEvent {
            host_slot: ReportedHostSlot::Unknown,
        },
    }
}

#[tokio::test]
async fn announcement_samples_pairing_at_event_time_and_preserves_unknown() {
    use super::ChangeHostCapture;
    use std::sync::atomic::{AtomicBool, Ordering};
    let changed = Arc::new(AtomicBool::new(false));
    let state = Arc::clone(&changed);
    let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        scripted_keyboard(
            request,
            if state.load(Ordering::SeqCst) {
                SlotStatus::AllPaired
            } else {
                SlotStatus::Reported
            },
        )
    });
    let channel = raw_scripted_channel(raw).await;
    let mut device = hidpp::device::Device::new(channel, 1).await.unwrap();
    let capture = ChangeHostCapture::resolve(CHANGE_HOST_INDEX, &mut device).await;
    assert_eq!(
        capture
            .departure_request(2)
            .await
            .unwrap()
            .keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Empty
        }
    );
    changed.store(true, Ordering::SeqCst);
    assert_eq!(
        capture
            .departure_request(2)
            .await
            .unwrap()
            .keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Paired
        }
    );

    let channel = scripted_channel(keyboard_without_hosts_info).await;
    let mut device = hidpp::device::Device::new(channel, 1).await.unwrap();
    let capture = ChangeHostCapture::resolve(CHANGE_HOST_INDEX, &mut device).await;
    assert_eq!(
        capture
            .departure_request(2)
            .await
            .unwrap()
            .keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown
        }
    );
}

#[tokio::test]
async fn departing_keyboard_slot_read_has_a_short_budget() {
    let channel = scripted_channel(keyboard_timing_out_on_slot_status).await;
    let mut device = hidpp::device::Device::new(channel, 1).await.unwrap();
    let capture = super::ChangeHostCapture::resolve(CHANGE_HOST_INDEX, &mut device).await;
    assert!(capture.slot_reader.is_supported());
    // The replay transport uses a blocking reader. Finish feature discovery
    // before pausing time so its real thread cannot lose to auto-advancement.
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let request = capture.departure_request(2).await.unwrap();
    let elapsed = tokio::time::Instant::now() - started;
    let budget = super::EVENT_SLOT_VALIDATION_TIMEOUT;
    // Tokio's timer wheel rounds deadlines up to the next millisecond.
    assert!((budget..=budget + std::time::Duration::from_millis(1)).contains(&elapsed));
    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown
        }
    );
}

#[tokio::test]
async fn queued_departure_announcement_survives_channel_retirement() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb369,
    };
    let node = NodeId::from("keyboard".to_owned());
    let registry = ChannelRegistry::default();
    registry.replace_node(
        node.clone(),
        [route.clone()],
        scripted_channel(keyboard_without_hosts_info).await,
    );
    let shared = registry.lookup(&route).unwrap();
    let (_stop, stopped) = tokio::sync::oneshot::channel();
    let (sender, mut presses) = tokio::sync::mpsc::unbounded_channel();
    let request = HostSwitchRequest {
        host: 2,
        keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        },
    };
    sender.send(request).unwrap();
    registry.remove_node(&node);
    let (_signal, gate) = device_io_channel();
    assert_eq!(
        super::monitor_host_switch(stopped, &mut presses, &registry, &shared, gate).await,
        super::HostSwitchStop::Pressed(request)
    );
}

#[test]
fn departure_decoder_rejects_replies_other_devices_and_invalid_slots() {
    let mut raw = [0_u8; 20];
    raw[0] = 0x11;
    raw[1] = 1;
    raw[2] = CHANGE_HOST_INDEX;
    raw[5] = 2;
    let decode = |bytes: &[u8]| {
        super::change_host_announcement(
            &hidpp::protocol::v20::Message::from(hidpp::channel::HidppMessage::Long(
                bytes[1..].try_into().unwrap(),
            )),
            1,
            CHANGE_HOST_INDEX,
            3,
        )
    };
    assert_eq!(decode(&raw), Some(2));
    raw[3] = 1;
    assert_eq!(decode(&raw), None);
    raw[3] = 0;
    raw[1] = 2;
    assert_eq!(decode(&raw), None);
    raw[1] = 1;
    raw[5] = 3;
    assert_eq!(decode(&raw), None);
}

#[tokio::test]
async fn departure_returns_request_and_retains_cleanup_until_reconnect() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb369,
    };
    let node = NodeId::from("keyboard".to_owned());
    let registry = ChannelRegistry::default();
    let (raw, departed_writes) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(
        node.clone(),
        [route.clone()],
        raw_scripted_channel(raw).await,
    );
    let shared = registry.lookup(&route).unwrap();
    let controls = crate::reprog_controls::ReprogControlsV4::new(
        Arc::clone(shared.channel()),
        shared.device_index(),
        9,
    );
    let request = HostSwitchRequest {
        host: 2,
        keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        },
    };
    let (_signal, gate) = device_io_channel();
    let outcome = super::finish_host_switch_session(
        super::HostSwitchStop::Pressed(request),
        &registry,
        &shared,
        &controls,
        vec![ArmedControl {
            cid: 0xd3,
            host: 2,
            mode: ReportingMode::Analytics,
            original: noisy_reporting(),
        }],
        None,
        &gate,
    )
    .await
    .unwrap();
    let (captured, pending) = outcome.into_parts();
    assert_eq!(captured, Some(request));
    assert!(
        departed_writes.written_reports().is_empty(),
        "forwarding must not wait on writes to a departing keyboard"
    );
    let pending = match pending.unwrap().retry(&registry).await {
        HostSwitchRestoreOutcome::RestorePending(pending) => pending,
        HostSwitchRestoreOutcome::Restored => {
            panic!("the departing publication must not be reused")
        }
    };
    let (raw, restored_writes) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(node, [route], raw_scripted_channel(raw).await);
    assert!(matches!(
        pending.retry(&registry).await,
        HostSwitchRestoreOutcome::Restored
    ));
    assert_eq!(restored_writes.written_reports().len(), 1);
}

#[test]
fn event_keeps_the_reporting_mode_of_the_exact_control() {
    let controls = [
        ArmedControl {
            cid: 0x00d3,
            host: 1,
            mode: ReportingMode::Diverted,
            original: noisy_reporting(),
        },
        ArmedControl {
            cid: 0x1234,
            host: 1,
            mode: ReportingMode::Analytics,
            original: noisy_reporting(),
        },
    ];
    let mut events = [AnalyticsKeyEvent::default(); 5];
    events[0] = AnalyticsKeyEvent {
        cid: ControlId(0x1234),
        event: 1,
    };
    assert_eq!(
        super::host_control_request(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events)),
        Some(HostSwitchRequest {
            host: 1,
            keyboard_transition: KeyboardHostTransition::AnalyticsEvent {
                host_slot: ReportedHostSlot::Unknown,
            },
        })
    );
}

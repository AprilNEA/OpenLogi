//! Host-switch capture and transition regressions.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use hidpp::{channel::HidppChannel, device::DeviceError, nibble::U4, protocol::v20};

use super::{
    ANALYTICS_CLEANUP_TIMEOUT, ArmedControl, ControlCleanup, HostSwitchError, HostSwitchRequest,
    KeyboardHostTransition, ReportedHostSlot, ReportingMode, change_host_announcement,
    finish_captured_control, host_change_required, host_control_request, prepare_host_change_on,
    resolve_change_host_capture, restoration_change, restore_host_controls_promptly,
    run_change_host_announcement_session, shares_channel, switch_linked_hosts,
};
use crate::channel::scripted::{
    ScriptedBackend, ScriptedNode, ScriptedRawHidChannel, ScriptedRawHidHandle, feature_error,
    scripted_node_info,
};
use crate::reprog_controls::{
    self, AnalyticsKeyEvent, CidReporting, ControlId, CtrlIdInfo, ReprogControlsEvent,
    ReprogControlsV4,
};
use crate::{ChannelPool, DeviceRoute, backend::BackendError};

mod capture;
mod controls;
mod slots;
mod transition;

use controls::analytics_request;

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
) -> (crate::backend::NodeInfo, ScriptedNode, ScriptedRawHidHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(responder);
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let mut node = scripted_node_info(id);
    node.product_id = product_id;
    (node, ScriptedNode::Live(channel), handle)
}

fn sent_host_change(handle: &ScriptedRawHidHandle, host: u8) -> bool {
    handle.written_reports().iter().any(|report| {
        report.get(2) == Some(&CHANGE_HOST_INDEX)
            && report.get(3).is_some_and(|function| function >> 4 == 1)
            && report.get(4) == Some(&host)
    })
}

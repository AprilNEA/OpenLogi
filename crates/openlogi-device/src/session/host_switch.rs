//! Keyboard-initiated host-switch synchronization.
//!
//! A session temporarily diverts the keyboard's three host controls, observes
//! which channel was pressed, switches the linked pointing devices, and then
//! switches the keyboard itself. Ordering matters: once the keyboard leaves
//! this host its HID++ channel can no longer command a mouse sharing the same
//! receiver.

use std::{future::Future, sync::Arc, time::Duration};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        change_host::ChangeHostFeature,
        hosts_info::{HostIndex, HostSlotStatus, HostsInfoFeature},
    },
    protocol::v20,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, info};

use crate::{
    ChannelPool, DeviceIoGate, DeviceRoute,
    backend::BackendError,
    reprog_controls::{self, ReprogControlsV4},
};

/// Why an armed host-switch session is being stopped externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSwitchStopReason {
    /// The keyboard remains reachable, so its controls must be restored.
    Graceful,
    /// The keyboard disappeared, so only local resources can be released.
    DeviceLost,
}

const HOST_CONTROL_IDS: [(reprog_controls::ControlId, u8); 3] = [
    (reprog_controls::control_ids::HOST_SWITCH_CHANNEL_1, 0),
    (reprog_controls::control_ids::HOST_SWITCH_CHANNEL_2, 1),
    (reprog_controls::control_ids::HOST_SWITCH_CHANNEL_3, 2),
];
const HOST_TASK_IDS: [(reprog_controls::TaskId, u8); 3] = [
    (reprog_controls::task_ids::HOST_SWITCH_CHANNEL_1, 0),
    (reprog_controls::task_ids::HOST_SWITCH_CHANNEL_2, 1),
    (reprog_controls::task_ids::HOST_SWITCH_CHANNEL_3, 2),
];
const HIDPP_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum ReportingMode {
    Diverted,
    Analytics,
}

#[derive(Clone, Copy)]
struct ArmedControl {
    cid: u16,
    host: u8,
    mode: ReportingMode,
    original: reprog_controls::CidReporting,
}

/// Failure while arming or running a host-switch link.
#[derive(Debug, Error)]
pub enum HostSwitchError {
    /// HID transport-level failure.
    #[error("HID transport error")]
    Hid(#[from] BackendError),
    /// The configured keyboard is not currently reachable.
    #[error("configured keyboard is not connected")]
    KeyboardNotFound,
    /// A configured target is not currently reachable.
    #[error("configured linked device is not connected")]
    TargetNotFound,
    /// A required HID++ operation failed.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
    /// A required HID++ operation did not complete within its budget.
    #[error("HID++ operation timed out while {operation}")]
    TimedOut {
        /// Description of the operation that exceeded its budget.
        operation: &'static str,
    },
    /// The keyboard cannot report its host switch controls to software.
    #[error("keyboard exposes no reportable host switch controls")]
    UnsupportedKeyboard,
    /// The device reports the requested host slot as unpaired, so switching to
    /// it would strand the device.
    #[error("host {host} is not paired on this device")]
    HostSlotEmpty {
        /// The zero-based host slot that has no pairing.
        host: u8,
    },
}

/// Capture host switch keys on `keyboard` until one is pressed or `shutdown`
/// resolves. Controls are restored before a requested host is returned.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    channel_pool: ChannelPool,
    mut device_io: DeviceIoGate,
) -> Result<Option<u8>, HostSwitchError> {
    if !device_io.allows_io() {
        return Err(HostSwitchError::Hid(BackendError::Backend(
            "host device I/O is suspended".into(),
        )));
    }
    let channel = open_channel(&channel_pool, &keyboard, "opening keyboard channel")
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let keyboard_index = keyboard.device_index();
    let device = timed_hidpp(
        "opening keyboard device",
        Device::new(Arc::clone(&channel), keyboard_index),
    )
    .await?;
    let feature = timed_hidpp(
        "locating host controls",
        device.root().get_feature(reprog_controls::FEATURE_ID),
    )
    .await?
    .ok_or(HostSwitchError::UnsupportedKeyboard)?;
    let controls = ReprogControlsV4::new(Arc::clone(&channel), keyboard_index, feature.index);

    let armed = arm_host_controls(&controls).await?;
    if armed.is_empty() {
        return Err(HostSwitchError::UnsupportedKeyboard);
    }

    let (press_tx, mut press_rx) = mpsc::unbounded_channel();
    let feature_index = controls.feature_index();
    let event_controls = armed.clone();
    let listener = channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        let Some(event) =
            reprog_controls::decode_full_event(&message, keyboard_index, feature_index)
        else {
            return;
        };
        if let Some(host) = event_host(&event_controls, event) {
            let _ = press_tx.send(host);
        }
    });

    info!(
        route = %keyboard,
        controls = armed.len(),
        "host switch link active"
    );
    let outcome = tokio::select! {
        reason = shutdown => {
            let reason = reason.unwrap_or(HostSwitchStopReason::DeviceLost);
            (None, reason == HostSwitchStopReason::Graceful)
        },
        Some(host) = press_rx.recv() => (Some(host), true),
    };

    drop(listener);
    if outcome.1 && device_io.wait_until_allowed().await {
        restore_host_controls(&controls, armed).await;
    }
    Ok(outcome.0)
}

/// Move reachable targets to `host`, then move the keyboard last.
///
/// Returns whether the keyboard actually changed hosts.
pub async fn switch_linked_hosts(
    keyboard: &DeviceRoute,
    targets: &[DeviceRoute],
    host: u8,
    channel_pool: &ChannelPool,
) -> Result<bool, HostSwitchError> {
    let channel = open_channel(channel_pool, keyboard, "opening keyboard channel")
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    // Validate the keyboard's own move before touching anything: preparation is
    // read-only, but it is the step that rejects an unpaired host slot, and
    // discovering that *after* the mice have moved would strand them on a host
    // the keyboard never reaches. Applying it still happens last, because once
    // the keyboard leaves this host its channel can no longer command a mouse
    // sharing the same receiver.
    let keyboard_change = prepare_host_change_on(&channel, keyboard.device_index(), host).await?;
    for target in targets {
        match prepare_host_change(target, host, keyboard, &channel, channel_pool).await {
            Ok(change) => {
                if let Err(error) = apply_host_change(change).await {
                    debug!(%error, route = %target, host, "linked device host switch failed");
                }
            }
            Err(error) => {
                debug!(%error, route = %target, host, "linked device host switch preparation failed");
            }
        }
    }
    let changed = apply_host_change(keyboard_change).await?;
    if changed {
        debug!(host, route = %keyboard, "keyboard host switched");
    }
    Ok(changed)
}

async fn arm_host_controls(
    controls: &ReprogControlsV4,
) -> Result<Vec<ArmedControl>, HostSwitchError> {
    let mut armed = Vec::new();
    if let Err(error) = arm_host_controls_inner(controls, &mut armed).await {
        restore_host_controls(controls, armed).await;
        return Err(error);
    }
    Ok(armed)
}

async fn arm_host_controls_inner(
    controls: &ReprogControlsV4,
    armed: &mut Vec<ArmedControl>,
) -> Result<(), HostSwitchError> {
    let count = timed_hidpp("reading host control count", controls.get_count()).await?;
    for index in 0..count {
        let info = timed_hidpp(
            "reading host control information",
            controls.get_ctrl_id_info(index),
        )
        .await?;
        let Some(host) = host_channel(info) else {
            continue;
        };
        debug!(
            cid = format_args!("{:#06x}", info.cid),
            task_id = format_args!("{:#06x}", info.task_id),
            host,
            divertable = info.is_divertable(),
            analytics = info.supports_analytics_events(),
            "host switch control discovered"
        );
        let mode = if info.is_divertable() {
            Some(ReportingMode::Diverted)
        } else if info.supports_analytics_events() {
            Some(ReportingMode::Analytics)
        } else {
            None
        };
        if let Some(mode) = mode {
            let original = timed_hidpp(
                "reading host control reporting",
                controls.get_cid_reporting(info.cid),
            )
            .await?;
            // Record the rollback before issuing the write: a transport timeout
            // can mean that the device applied the request but its response was
            // lost, so the failing control must be restored as well.
            armed.push(ArmedControl {
                cid: info.cid,
                host,
                mode,
                original,
            });
            match mode {
                ReportingMode::Diverted => {
                    timed_hidpp("diverting host control", controls.divert_cid(info.cid)).await?;
                }
                ReportingMode::Analytics => {
                    timed_hidpp(
                        "enabling host control analytics",
                        controls.set_cid_reporting_full(
                            info.cid,
                            reprog_controls::CidReportingChange {
                                analytics_key_events: Some(true),
                                ..reprog_controls::CidReportingChange::default()
                            },
                        ),
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn restore_host_controls(controls: &ReprogControlsV4, armed: Vec<ArmedControl>) {
    for control in armed {
        let mut restored = restore_host_control(controls, control).await;
        if restored.is_err() {
            restored = restore_host_control(controls, control).await;
        }
        if let Err(error) = restored {
            debug!(
                ?error,
                cid = control.cid,
                "could not restore host switch control"
            );
        }
    }
}

async fn restore_host_control(
    controls: &ReprogControlsV4,
    control: ArmedControl,
) -> Result<(), HostSwitchError> {
    timed_hidpp(
        "restoring host control reporting",
        controls.set_cid_reporting_full(control.cid, restoration_change(control)),
    )
    .await
    .map(|_echo| ())
}

fn restoration_change(control: ArmedControl) -> reprog_controls::CidReportingChange {
    match control.mode {
        ReportingMode::Diverted => reprog_controls::CidReportingChange {
            diverted: Some(control.original.diverted),
            raw_xy: Some(control.original.raw_xy),
            ..reprog_controls::CidReportingChange::default()
        },
        ReportingMode::Analytics => reprog_controls::CidReportingChange {
            analytics_key_events: Some(control.original.analytics_key_events),
            ..reprog_controls::CidReportingChange::default()
        },
    }
}

struct PreparedHostChange {
    feature: Arc<ChangeHostFeature>,
    device_index: u8,
    host: u8,
    required: bool,
}

async fn prepare_host_change(
    target: &DeviceRoute,
    host: u8,
    keyboard: &DeviceRoute,
    keyboard_channel: &Arc<HidppChannel>,
    channel_pool: &ChannelPool,
) -> Result<PreparedHostChange, HostSwitchError> {
    if shares_channel(target, keyboard) {
        prepare_host_change_on(keyboard_channel, target.device_index(), host).await
    } else {
        let channel = open_channel(channel_pool, target, "opening linked device channel")
            .await?
            .ok_or(HostSwitchError::TargetNotFound)?;
        prepare_host_change_on(&channel, target.device_index(), host).await
    }
}

async fn prepare_host_change_on(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    host: u8,
) -> Result<PreparedHostChange, HostSwitchError> {
    let mut device = timed_hidpp(
        "opening host-change device",
        Device::new(Arc::clone(channel), device_index),
    )
    .await?;
    let info = timed_hidpp(
        "locating host-change feature",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await?
    .ok_or_else(|| HostSwitchError::Hidpp("ChangeHost is unsupported".into()))?;
    let change_host = device.add_feature::<ChangeHostFeature>(info.index);
    let state = timed_hidpp("reading current host", change_host.get_host_info()).await?;
    let required = host_change_required(state.current_host, state.host_count, host)?;
    if required && host_slot_is_empty(&mut device, host).await {
        return Err(HostSwitchError::HostSlotEmpty { host });
    }
    Ok(PreparedHostChange {
        feature: change_host,
        device_index,
        host,
        required,
    })
}

async fn apply_host_change(change: PreparedHostChange) -> Result<bool, HostSwitchError> {
    if !change.required {
        let PreparedHostChange {
            device_index, host, ..
        } = change;
        debug!(device_index, host, "device already uses requested host");
        return Ok(false);
    }
    timed_hidpp(
        "writing current host",
        change.feature.set_current_host(change.host),
    )
    .await?;
    Ok(true)
}

async fn open_channel(
    channel_pool: &ChannelPool,
    route: &DeviceRoute,
    operation: &'static str,
) -> Result<Option<Arc<HidppChannel>>, HostSwitchError> {
    timeout(HIDPP_OPERATION_TIMEOUT, channel_pool.open(route))
        .await
        .map_err(|_| HostSwitchError::TimedOut { operation })?
        .map_err(HostSwitchError::Hid)
}

async fn timed_hidpp<T, E>(
    operation: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, HostSwitchError>
where
    E: std::fmt::Debug,
{
    timeout(HIDPP_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| HostSwitchError::TimedOut { operation })?
        .map_err(|error| hidpp_error(operation, error))
}

/// Whether the device explicitly reports `host` as an empty slot.
///
/// `ChangeHost`'s `host_count` counts the device's RF channels, not the ones
/// that have a pairing. Switching to an empty slot is not refused by the
/// device: `setCurrentHost` is fire-and-forget and a successful switch usually
/// resets the device, so it simply drops off this host and does not come back
/// until the user pairs that slot or presses the device's own host button. A
/// keyboard with three host keys paired to two machines is enough to hit this.
///
/// `HostsInfo` (`0x1815`) is the only feature that reports per-slot pairing
/// status, and asking is advisory: a device that does not implement it, times
/// out, returns a feature error, or answers with a status byte outside the
/// spec has not said the slot is empty, and must still be allowed to switch.
/// Only an explicit `Empty` refuses, so this returns a plain `bool` — an
/// unreadable status can never abort the transition it was meant to protect.
async fn host_slot_is_empty(device: &mut Device, host: u8) -> bool {
    let feature = timed_hidpp(
        "locating hosts-info feature",
        device.root().get_feature(HostsInfoFeature::ID),
    )
    .await;
    let index = match feature {
        Ok(Some(info)) => info.index,
        Ok(None) => return false,
        Err(error) => {
            debug!(host, %error, "hosts-info lookup failed; treating the slot as usable");
            return false;
        }
    };
    let hosts_info = device.add_feature::<HostsInfoFeature>(index);
    match timed_hidpp(
        "reading host slot status",
        hosts_info.get_host_info(HostIndex::Slot(host)),
    )
    .await
    {
        Ok(slot) => slot.status == HostSlotStatus::Empty,
        Err(error) => {
            debug!(host, %error, "host slot status is unreadable; treating the slot as usable");
            false
        }
    }
}

fn host_change_required(
    current_host: u8,
    host_count: u8,
    requested_host: u8,
) -> Result<bool, HostSwitchError> {
    if requested_host >= host_count {
        return Err(HostSwitchError::Hidpp(format!(
            "host {requested_host} is outside device host count {host_count}"
        )));
    }
    Ok(current_host != requested_host)
}

fn shares_channel(left: &DeviceRoute, right: &DeviceRoute) -> bool {
    left.shares_transport(right)
}

fn hidpp_error(operation: &'static str, error: impl std::fmt::Debug) -> HostSwitchError {
    HostSwitchError::Hidpp(format!("{operation}: {error:?}"))
}

fn host_channel(info: reprog_controls::CtrlIdInfo) -> Option<u8> {
    HOST_CONTROL_IDS
        .iter()
        .find_map(|(cid, host)| (info.cid == cid.0).then_some(*host))
        .or_else(|| {
            HOST_TASK_IDS
                .iter()
                .find_map(|(task, host)| (info.task_id == task.0).then_some(*host))
        })
}

fn event_host(
    controls: &[ArmedControl],
    event: reprog_controls::ReprogControlsEvent,
) -> Option<u8> {
    match event {
        reprog_controls::ReprogControlsEvent::DivertedButtons(cids) => controls
            .iter()
            .find_map(|control| cids.contains(&control.cid.into()).then_some(control.host)),
        reprog_controls::ReprogControlsEvent::AnalyticsKeyEvents(events) => {
            controls.iter().find_map(|control| {
                events
                    .iter()
                    .any(|event| event.cid.0 == control.cid)
                    .then_some(control.host)
            })
        }
        reprog_controls::ReprogControlsEvent::DivertedRawMouseXy { .. }
        | reprog_controls::ReprogControlsEvent::DivertedRawWheel { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hidpp::channel::HidppChannel;

    use super::{
        ArmedControl, HostSwitchError, ReportingMode, event_host, host_change_required,
        host_channel, prepare_host_change_on, restoration_change, shares_channel,
    };
    use crate::DeviceRoute;
    use crate::channel::scripted::{ScriptedRawHidChannel, feature_error};
    use crate::reprog_controls::{
        AnalyticsKeyEvent, CidReporting, ControlId, CtrlIdInfo, ReprogControlsEvent,
    };

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
        /// Reports the feature as unimplemented, the usual index-0 lookup miss.
        Unimplemented,
        /// Errors on the lookup itself, as firmware that refuses unknown
        /// feature ids rather than reporting index 0 does.
        LookupErrors,
        /// Implements the feature but errors on the status read.
        ReadErrors,
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
                        SlotStatus::Reported | SlotStatus::ReadErrors => HOSTS_INFO_INDEX,
                    },
                    _ => 0x00,
                };
            }
            // ChangeHost getHostInfo: three RF channels, currently on host 0.
            (CHANGE_HOST_INDEX, 0x00) => payload[..2].copy_from_slice(&[3, 0]),
            // HostsInfo getHostInfo: echo the slot, then its pairing status.
            (HOSTS_INFO_INDEX, 0x01) => {
                if slot_status == SlotStatus::ReadErrors {
                    return Some(feature_error(request, BUSY));
                }
                payload[0] = request[4];
                payload[1] = u8::from(request[4] < 2);
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
            event_host(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events)),
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
}

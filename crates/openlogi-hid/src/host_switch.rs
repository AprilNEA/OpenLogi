//! Keyboard-initiated host-switch synchronization.
//!
//! A session temporarily diverts the keyboard's three host controls, observes
//! which channel was pressed, switches the linked pointing devices, and then
//! switches the keyboard itself. Ordering matters: once the keyboard leaves
//! this host its HID++ channel can no longer command a mouse sharing the same
//! receiver.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{CreatableFeature, change_host::ChangeHostFeature},
    protocol::v20,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use crate::{
    ChannelPool,
    reprog_controls::{self, ReprogControlsV4},
    route::DeviceRoute,
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
}

/// Failure while arming or running a host-switch link.
#[derive(Debug, Error)]
pub enum HostSwitchError {
    /// HID transport-level failure.
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
    /// The configured keyboard is not currently reachable.
    #[error("configured keyboard is not connected")]
    KeyboardNotFound,
    /// A configured target is not currently reachable.
    #[error("configured linked device is not connected")]
    TargetNotFound,
    /// A required HID++ operation failed.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
    /// The keyboard cannot report its host switch controls to software.
    #[error("keyboard exposes no reportable host switch controls")]
    UnsupportedKeyboard,
}

/// Capture host switch keys on `keyboard` until one is pressed or `shutdown`
/// resolves. Controls are restored before a requested host is returned.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    channel_pool: ChannelPool,
) -> Result<Option<u8>, HostSwitchError> {
    let channel = channel_pool
        .open(&keyboard)
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let keyboard_index = keyboard.device_index();
    let device = Device::new(Arc::clone(&channel), keyboard_index)
        .await
        .map_err(hidpp_error)?;
    let feature = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(hidpp_error)?
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
    if outcome.1 {
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
    let channel = channel_pool
        .open(keyboard)
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    for target in targets {
        if let Err(error) = set_host(target, host, keyboard, &channel, channel_pool).await {
            debug!(%error, route = %target, host, "linked device host switch failed");
        }
    }
    let changed = set_host_on(&channel, keyboard.device_index(), host).await?;
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
    let count = controls.get_count().await.map_err(hidpp_error)?;
    for index in 0..count {
        let info = controls
            .get_ctrl_id_info(index)
            .await
            .map_err(hidpp_error)?;
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
            // Record the rollback before issuing the write: a transport timeout
            // can mean that the device applied the request but its response was
            // lost, so the failing control must be restored as well.
            armed.push(ArmedControl {
                cid: info.cid,
                host,
                mode,
            });
            match mode {
                ReportingMode::Diverted => controls
                    .set_cid_reporting(info.cid, true, false)
                    .await
                    .map_err(hidpp_error)?,
                ReportingMode::Analytics => {
                    controls
                        .set_cid_reporting_full(
                            info.cid,
                            reprog_controls::CidReportingChange {
                                analytics_key_events: Some(true),
                                ..reprog_controls::CidReportingChange::default()
                            },
                        )
                        .await
                        .map_err(hidpp_error)?;
                }
            }
        }
    }
    Ok(())
}

async fn restore_host_controls(controls: &ReprogControlsV4, armed: Vec<ArmedControl>) {
    for control in armed {
        let restored = match control.mode {
            ReportingMode::Diverted => controls.set_cid_reporting(control.cid, false, false).await,
            ReportingMode::Analytics => controls
                .set_cid_reporting_full(
                    control.cid,
                    reprog_controls::CidReportingChange {
                        analytics_key_events: Some(false),
                        ..reprog_controls::CidReportingChange::default()
                    },
                )
                .await
                .map(|_echo| ()),
        };
        if let Err(error) = restored {
            debug!(
                ?error,
                cid = control.cid,
                "could not restore host switch control"
            );
        }
    }
}

async fn set_host(
    target: &DeviceRoute,
    host: u8,
    keyboard: &DeviceRoute,
    keyboard_channel: &Arc<HidppChannel>,
    channel_pool: &ChannelPool,
) -> Result<(), HostSwitchError> {
    if shares_channel(target, keyboard) {
        set_host_on(keyboard_channel, target.device_index(), host)
            .await
            .map(|_| ())
    } else {
        let channel = channel_pool
            .open(target)
            .await?
            .ok_or(HostSwitchError::TargetNotFound)?;
        set_host_on(&channel, target.device_index(), host)
            .await
            .map(|_| ())
    }
}

async fn set_host_on(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    host: u8,
) -> Result<bool, HostSwitchError> {
    let mut device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(hidpp_error)?;
    let info = device
        .root()
        .get_feature(ChangeHostFeature::ID)
        .await
        .map_err(hidpp_error)?
        .ok_or_else(|| HostSwitchError::Hidpp("ChangeHost is unsupported".into()))?;
    let change_host = device.add_feature::<ChangeHostFeature>(info.index);
    let state = change_host.get_host_info().await.map_err(hidpp_error)?;
    if !host_change_required(state.current_host, state.host_count, host)? {
        debug!(device_index, host, "device already uses requested host");
        return Ok(false);
    }
    change_host
        .set_current_host(host)
        .await
        .map_err(hidpp_error)?;
    Ok(true)
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

fn hidpp_error(error: impl std::fmt::Debug) -> HostSwitchError {
    HostSwitchError::Hidpp(format!("{error:?}"))
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
    use super::{
        ArmedControl, ReportingMode, event_host, host_change_required, host_channel, shares_channel,
    };
    use crate::DeviceRoute;
    use crate::reprog_controls::{AnalyticsKeyEvent, ControlId, CtrlIdInfo, ReprogControlsEvent};

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
        assert!(host_change_required(0, 2, 2).is_err());
    }
}

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
    feature::{CreatableFeature, change_host::ChangeHostFeature},
    protocol::v20,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
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
    /// A required HID++ operation did not complete within its budget.
    #[error("HID++ operation timed out while {operation}")]
    TimedOut {
        /// Description of the operation that exceeded its budget.
        operation: &'static str,
    },
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
    let channel = open_channel(channel_pool, keyboard, "opening keyboard channel")
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let mut prepared_targets = Vec::with_capacity(targets.len());
    for target in targets {
        prepared_targets
            .push(prepare_host_change(target, host, keyboard, &channel, channel_pool).await?);
    }
    let keyboard_change = prepare_host_change_on(&channel, keyboard.device_index(), host).await?;
    for change in prepared_targets {
        apply_host_change(change).await?;
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
                    timed_hidpp(
                        "diverting host control",
                        controls.set_cid_reporting(info.cid, true, false),
                    )
                    .await?;
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
        let restored = timed_hidpp(
            "restoring host control reporting",
            controls.set_cid_reporting_full(control.cid, restoration_change(control)),
        )
        .await
        .map(|_echo| ());
        if let Err(error) = restored {
            debug!(
                ?error,
                cid = control.cid,
                "could not restore host switch control"
            );
        }
    }
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
        .map_err(hidpp_error)
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
        ArmedControl, ReportingMode, event_host, host_change_required, host_channel,
        restoration_change, shares_channel,
    };
    use crate::DeviceRoute;
    use crate::reprog_controls::{
        AnalyticsKeyEvent, CidReporting, ControlId, CtrlIdInfo, ReprogControlsEvent,
    };

    fn reporting(diverted: bool, raw_xy: bool, analytics_key_events: bool) -> CidReporting {
        CidReporting {
            cid: ControlId(0x00d3),
            diverted,
            persistently_diverted: true,
            force_raw_xy: true,
            raw_xy,
            remap: Some(ControlId(0x1234)),
            analytics_key_events,
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
            original: reporting(false, false, false),
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

    #[test]
    fn diverted_cleanup_restores_only_the_original_temporary_bits() {
        let change = restoration_change(ArmedControl {
            cid: 0x00d3,
            host: 2,
            mode: ReportingMode::Diverted,
            original: reporting(true, true, false),
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
            original: reporting(false, false, true),
        });

        assert_eq!(change.analytics_key_events, Some(true));
        assert_eq!(change.diverted, None);
        assert_eq!(change.raw_xy, None);
    }
}

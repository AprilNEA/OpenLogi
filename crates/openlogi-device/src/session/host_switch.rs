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

mod restore;

use restore::rollback_host_switch_start;
pub use restore::{
    HostSwitchRestoreOutcome, HostSwitchSessionFailure, HostSwitchSessionOutcome,
    PendingHostSwitchRestore,
};

use crate::{
    ChannelPool, ChannelRegistry, DeviceIoGate, DeviceRoute, IoSuspended, SharedChannel,
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

impl From<IoSuspended> for HostSwitchError {
    fn from(error: IoSuspended) -> Self {
        Self::Hid(error.into())
    }
}

/// Capture host switch keys until a press, shutdown, or channel retirement.
///
/// Returns any requested host together with the restoration outcome. The caller
/// must retain pending restoration and finish it before switching hosts or
/// starting a successor session.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    registry: &ChannelRegistry,
    device_io: DeviceIoGate,
) -> Result<HostSwitchSessionOutcome, HostSwitchSessionFailure> {
    device_io.ensure_allowed().map_err(HostSwitchError::from)?;
    let shared = registry
        .lookup(&keyboard)
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let channel = Arc::clone(shared.channel());
    let keyboard_index = shared.device_index();
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

    let mut armed = Vec::new();
    if let Err(error) = arm_host_controls_inner(&controls, &mut armed).await {
        let pending = PendingHostSwitchRestore::new(&shared, controls.feature_index(), armed);
        return Err(rollback_host_switch_start(error, pending, registry, &device_io).await);
    }
    if armed.is_empty() {
        return Err(HostSwitchError::UnsupportedKeyboard.into());
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
    let stop = monitor_host_switch(
        shutdown,
        &mut press_rx,
        registry,
        &shared,
        device_io.clone(),
    )
    .await;

    drop(listener);
    let requested_host = stop.requested_host();
    let Some(mut pending) = PendingHostSwitchRestore::new(&shared, controls.feature_index(), armed)
    else {
        return Ok(HostSwitchSessionOutcome::Restored { requested_host });
    };
    let reuse_armed_channel = match stop {
        // A press does not retire the channel: teardown may write through it
        // for as long as inventory still publishes it.
        HostSwitchStop::Pressed(_) => registry.is_current(&shared),
        HostSwitchStop::Shutdown => true,
        HostSwitchStop::ChannelChanged => false,
    };
    if reuse_armed_channel {
        pending = pending.allow_current_channel();
    }
    if !device_io.allows_io() {
        return Ok(HostSwitchSessionOutcome::RestorePending {
            requested_host,
            restore: pending,
        });
    }
    Ok(match pending.retry(registry).await {
        HostSwitchRestoreOutcome::Restored => HostSwitchSessionOutcome::Restored { requested_host },
        HostSwitchRestoreOutcome::RestorePending(restore) => {
            HostSwitchSessionOutcome::RestorePending {
                requested_host,
                restore,
            }
        }
    })
}

/// Why monitoring an armed host-switch session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostSwitchStop {
    /// The keyboard asked for this zero-based host.
    Pressed(u8),
    /// Teardown was requested while inventory still published the channel
    /// that armed the session.
    Shutdown,
    /// The channel that armed the session must not be written through again:
    /// inventory removed or replaced it, or the owner reported the keyboard
    /// lost.
    ChannelChanged,
}

impl HostSwitchStop {
    fn requested_host(self) -> Option<u8> {
        match self {
            Self::Pressed(host) => Some(host),
            Self::Shutdown | Self::ChannelChanged => None,
        }
    }

    /// A stop that did not itself retire the channel. Re-checking inventory
    /// keeps a simultaneously ready replacement from being written underneath.
    fn for_current_publication(registry: &ChannelRegistry, shared: &SharedChannel) -> Self {
        if registry.is_current(shared) {
            Self::Shutdown
        } else {
            Self::ChannelChanged
        }
    }
}

async fn monitor_host_switch(
    mut shutdown: oneshot::Receiver<HostSwitchStopReason>,
    presses: &mut mpsc::UnboundedReceiver<u8>,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    mut device_io: DeviceIoGate,
) -> HostSwitchStop {
    let mut registry_changes = registry.subscribe();
    loop {
        if !registry.is_current(shared) {
            info!(route = %shared.route(), "inventory replaced or removed host-switch channel");
            return HostSwitchStop::ChannelChanged;
        }
        tokio::select! {
            biased;

            changed = registry_changes.changed() => {
                if changed.is_err() {
                    return HostSwitchStop::ChannelChanged;
                }
            }
            reason = &mut shutdown => {
                return match reason.unwrap_or(HostSwitchStopReason::DeviceLost) {
                    HostSwitchStopReason::Graceful => {
                        HostSwitchStop::for_current_publication(registry, shared)
                    }
                    HostSwitchStopReason::DeviceLost => HostSwitchStop::ChannelChanged,
                };
            }
            host = presses.recv() => {
                return match host {
                    Some(host) => HostSwitchStop::Pressed(host),
                    None => HostSwitchStop::for_current_publication(registry, shared),
                };
            }
            allowed = device_io.changed() => {
                if allowed.is_none() {
                    return HostSwitchStop::for_current_publication(registry, shared);
                }
            }
        }
    }
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

async fn restore_host_controls(controls: &ReprogControlsV4, armed: &[ArmedControl]) -> bool {
    let mut complete = true;
    for &control in armed {
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
            complete = false;
        }
    }
    complete
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
mod tests;

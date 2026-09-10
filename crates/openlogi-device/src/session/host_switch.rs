//! Keyboard-initiated host-switch synchronization.
//!
//! A session temporarily diverts the keyboard's three host controls, observes
//! which channel was pressed, switches the linked pointing devices, and then
//! switches the keyboard itself. Ordering matters: once the keyboard leaves
//! this host its HID++ channel can no longer command a mouse sharing the same
//! receiver.
//!
//! Not every keyboard can be driven that way. Some report their host controls
//! as plain hotkeys — `0x1b04` marks them neither divertable nor backed by a
//! working analytics stream — and switch host themselves the moment one is
//! pressed. Those announce the change on `0x1814` instead, roughly 150 ms
//! before they drop off the bus, and the session follows that announcement
//! with the targets alone.

use std::{future::Future, sync::Arc, time::Duration};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        change_host::{ChangeHostEvent, ChangeHostFeature, decode_event as decode_announcement},
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
    ChannelPool, ChannelRegistry, DeviceIoGate, DeviceRoute, SharedChannel,
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

/// One requested host change.
///
/// The two variants exist because the two ways a keyboard reports a host switch
/// carry different information. A control the firmware lets us divert (or
/// report as an analytics key event) names the destination, and the keyboard is
/// still present to be moved. `0x1814` instead announces a switch the keyboard
/// performs on its own, and names the host being *left*, so the destination has
/// to come from each target's own pairing table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSwitchRequest {
    /// A diverted or analytics control named `host` as the destination.
    Directed(u8),
    /// `0x1814` announced a switch the keyboard performs itself, away from
    /// `leader_host`.
    ///
    /// The host comes from the keyboard's own `0x1814` state read while arming,
    /// not from the notification: the notification does name a host, but which
    /// one it names could not be established (every capture had the keyboard on
    /// the same host, so "the host being left" and "a constant" fit the bytes
    /// equally well), and the host it sat on when armed is the one it leaves.
    Announced {
        /// Host the keyboard is leaving.
        leader_host: u8,
    },
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
    if !device_io.allows_io() {
        return Err(HostSwitchError::Hid(BackendError::Backend(
            "host device I/O is suspended".into(),
        ))
        .into());
    }
    let shared = registry
        .lookup(&keyboard)
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let channel = Arc::clone(shared.channel());
    let keyboard_index = shared.device_index();
    let mut device = timed_hidpp(
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
    let announcement = announcement_state(&mut device).await;

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
        if let Some(request) = decode_request(
            &message,
            keyboard_index,
            feature_index,
            &event_controls,
            announcement,
        ) {
            let _ = press_tx.send(request);
        }
    });

    info!(
        route = %keyboard,
        controls = armed.len(),
        // Whether this session can hear an announcement at all. A keyboard that
        // cannot divert its host controls depends entirely on this, and the two
        // reads behind it can fail against a device that is still settling after
        // a reconnect, so it is stated on every arm rather than assumed.
        announces = announcement.is_some(),
        "host switch link active"
    );
    let (request, permit_retired_channel) = monitor_host_switch(
        shutdown,
        &mut press_rx,
        registry,
        &shared,
        device_io.clone(),
    )
    .await;

    if let Some(HostSwitchRequest::Announced { leader_host }) = request {
        debug!(route = %keyboard, leader_host, "keyboard announced a host change");
    }
    let requested_host = request;

    drop(listener);
    // An announcement leaves nothing to restore. The controls were set on a
    // device that has since gone, so the write can only spend the HID++ timeout
    // and then retry against something absent; the keyboard arms from scratch
    // when it comes back. Returning here also drops the session's shared
    // receiver lease at once, and that lease is what the linked devices are
    // queued behind: every second spent here is a second the pointer stays on
    // the machine the user just left.
    if matches!(requested_host, Some(HostSwitchRequest::Announced { .. })) {
        return Ok(HostSwitchSessionOutcome::Restored { requested_host });
    }
    let Some(mut pending) = PendingHostSwitchRestore::new(&shared, controls.feature_index(), armed)
    else {
        return Ok(HostSwitchSessionOutcome::Restored { requested_host });
    };
    if permit_retired_channel {
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

async fn monitor_host_switch(
    mut shutdown: oneshot::Receiver<HostSwitchStopReason>,
    presses: &mut mpsc::UnboundedReceiver<HostSwitchRequest>,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    mut device_io: DeviceIoGate,
) -> (Option<HostSwitchRequest>, bool) {
    let mut registry_changes = registry.subscribe();
    loop {
        if !registry.is_current(shared) {
            info!(route = %shared.route(), "inventory replaced or removed host-switch channel");
            return (None, false);
        }
        tokio::select! {
            biased;

            changed = registry_changes.changed() => {
                if changed.is_err() {
                    return (None, false);
                }
            }
            reason = &mut shutdown => {
                let reason = reason.unwrap_or(HostSwitchStopReason::DeviceLost);
                let current = registry.is_current(shared);
                return (
                    None,
                    reason == HostSwitchStopReason::Graceful && current,
                );
            }
            host = presses.recv() => {
                return (host, registry.is_current(shared));
            }
            allowed = device_io.changed() => {
                if allowed.is_none() {
                    return (None, registry.is_current(shared));
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
    request: HostSwitchRequest,
    channel_pool: &ChannelPool,
) -> Result<bool, HostSwitchError> {
    // Handled before the keyboard's channel is opened, because by now there is
    // no keyboard to open one to: it announced a switch it performs itself and
    // has already left. Opening it first fails with `KeyboardNotFound` and takes
    // the targets down with it.
    let host = match request {
        HostSwitchRequest::Directed(host) => host,
        HostSwitchRequest::Announced { leader_host } => {
            follow_announced_change(targets, leader_host, channel_pool).await;
            return Ok(false);
        }
    };
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

/// The request a raw report carries, or `None` when it carries neither.
///
/// The two paths are tried in order because they are not alternatives so much
/// as a preference: a keyboard that lets its host controls be diverted reports
/// through `0x1b04` and names the destination, which is strictly better than an
/// announcement that does not. `0x1814` is what is left for the keyboards that
/// refuse.
fn decode_request(
    message: &v20::Message,
    keyboard_index: u8,
    feature_index: u8,
    controls: &[ArmedControl],
    announcement: Option<(u8, u8)>,
) -> Option<HostSwitchRequest> {
    if let Some(event) = reprog_controls::decode_full_event(message, keyboard_index, feature_index)
    {
        return event_host(controls, event).map(HostSwitchRequest::Directed);
    }
    let (index, leader_host) = announcement?;
    decode_announcement(message, keyboard_index, index)
        .is_some_and(|event| matches!(event, ChangeHostEvent::HostChange { .. }))
        .then_some(HostSwitchRequest::Announced { leader_host })
}

/// What an announcement needs, read while the keyboard is still unhurried: the
/// `0x1814` feature index to listen on, and the host it is sitting on.
///
/// Both are read now because neither can be read later. The announcement only
/// arrives once the key has been pressed, and by then the keyboard is on its
/// way out. The host matters as much as the index: the notification names a
/// host, but which one it names could not be established here (every capture
/// had the keyboard on the same host, so "the host being left" and "a constant"
/// fit the bytes equally well), whereas the host the keyboard sits on at arming
/// time is the host it leaves, by definition.
///
/// `None` for a keyboard without the feature, which simply never announces.
async fn announcement_state(device: &mut Device) -> Option<(u8, u8)> {
    let info = timed_hidpp(
        "locating host-change feature",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await
    .ok()
    .flatten()?;
    let change_host = device.add_feature::<ChangeHostFeature>(info.index);
    let state = timed_hidpp("reading current host", change_host.get_host_info())
        .await
        .ok()?;
    Some((info.index, state.current_host))
}

/// Moves every target to the host it can follow the keyboard to.
///
/// `0x1814` names the host being left, never the one being joined, so the
/// destination is whatever single *other* host the target is still paired to. A
/// target paired to two or more of them is ambiguous, and guessing would strand
/// it on a machine the keyboard never reached, so it is left where it is.
async fn follow_announced_change(
    targets: &[DeviceRoute],
    leader_host: u8,
    channel_pool: &ChannelPool,
) {
    for target in targets {
        let channel =
            match open_channel(channel_pool, target, "opening linked device channel").await {
                Ok(Some(channel)) => channel,
                Ok(None) => {
                    debug!(route = %target, "linked device is out of reach; leaving it alone");
                    continue;
                }
                Err(error) => {
                    debug!(%error, route = %target, "could not open the linked device");
                    continue;
                }
            };
        match sole_followable_host(&channel, target, leader_host).await {
            Ok(Some(host)) => {
                match prepare_host_change_on(&channel, target.device_index(), host).await {
                    Ok(change) => match apply_host_change(change).await {
                        Ok(_) => {
                            info!(route = %target, host, "linked device followed the keyboard");
                        }
                        Err(error) => {
                            debug!(%error, route = %target, host, "announced host switch failed");
                        }
                    },
                    Err(error) => {
                        debug!(%error, route = %target, host, "announced host switch preparation failed");
                    }
                }
            }
            Ok(None) => {
                debug!(
                    route = %target,
                    leader_host,
                    "nowhere unambiguous to follow to; leaving the device where it is"
                );
            }
            Err(error) => {
                debug!(%error, route = %target, leader_host, "could not read the target host table");
            }
        }
    }
}

/// The one host `target` is paired to besides the one it currently sits on.
///
/// Returns `None` when the device has no other paired host, or more than one:
/// both mean the announcement alone cannot say where it should go.
async fn sole_followable_host(
    channel: &Arc<HidppChannel>,
    target: &DeviceRoute,
    leader_host: u8,
) -> Result<Option<u8>, HostSwitchError> {
    let mut device = timed_hidpp(
        "opening host-change device",
        Device::new(Arc::clone(channel), target.device_index()),
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

    // A target that is not on the host being left has nothing to follow. It may
    // have been switched on its own, and the announcement says only that the
    // keyboard left one machine — never which one it joined — so moving this
    // device could just as easily drag it back to the machine being abandoned.
    if state.current_host != leader_host {
        return Ok(None);
    }
    let mut paired = Vec::new();
    for host in 0..state.host_count {
        if host_slot_is_paired(&mut device, host).await {
            paired.push(host);
        }
    }
    Ok(sole_other_host(leader_host, &paired))
}

/// Whether `host` is *confirmed* paired on `device`.
///
/// [`host_slot_is_empty`] answers the opposite question and treats an
/// unreadable slot as usable, which is right where the user named the
/// destination: refusing on a failed read would break a switch that would have
/// worked. Here the destination is being chosen rather than obeyed, and a slot
/// nothing confirms is paired is a slot a device gets stranded on, so an
/// unreadable one is not a candidate.
async fn host_slot_is_paired(device: &mut Device, host: u8) -> bool {
    let Ok(Some(info)) = timed_hidpp(
        "locating hosts-info feature",
        device.root().get_feature(HostsInfoFeature::ID),
    )
    .await
    else {
        return false;
    };
    let hosts_info = device.add_feature::<HostsInfoFeature>(info.index);
    matches!(
        timed_hidpp(
            "reading host slot status",
            hosts_info.get_host_info(HostIndex::Slot(host)),
        )
        .await,
        Ok(slot) if slot.status == HostSlotStatus::Paired
    )
}

/// The single host in `paired` that is not `leaving`.
///
/// `None` when there is no such host, or more than one: an announcement says
/// only that the keyboard left, so with two candidates there is nothing to
/// separate the machine it went to from the one it did not, and moving a
/// pointing device on a coin flip strands it half the time.
fn sole_other_host(leaving: u8, paired: &[u8]) -> Option<u8> {
    let mut others = paired.iter().copied().filter(|host| *host != leaving);
    let first = others.next()?;
    others.next().is_none().then_some(first)
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
mod tests {
    use std::sync::Arc;

    use hidpp::channel::HidppChannel;

    use super::{
        ArmedControl, HostSwitchError, HostSwitchRequest, HostSwitchRestoreOutcome,
        PendingHostSwitchRestore, ReportingMode, decode_request, event_host, host_change_required,
        host_channel, prepare_host_change_on, restoration_change, rollback_host_switch_start,
        shares_channel, sole_other_host,
    };
    use crate::backend::NodeId;
    use crate::channel::scripted::{
        ScriptedRawHidChannel, feature_error, scripted_channel as raw_scripted_channel,
    };
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

    /// The report an MX Keys S sends about 150 ms before it leaves, taken from a
    /// `hidraw` capture: device index `0xff`, feature index `0x0a`, function 0,
    /// software id 0. The keyboard was on host 1 and the key pressed targeted
    /// host 0, which is how the payload byte is known not to be the destination.
    const ANNOUNCEMENT: [u8; LONG_REPORT_LENGTH - 1] = [
        0xff, 0x0a, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    use hidpp::channel::{HidppMessage, LONG_REPORT_LENGTH};
    use hidpp::protocol::v20;

    fn announcement_message() -> v20::Message {
        v20::Message::from(HidppMessage::Long(ANNOUNCEMENT))
    }

    #[test]
    fn a_captured_announcement_decodes_into_a_follow_request() {
        assert_eq!(
            decode_request(&announcement_message(), 0xff, 0x09, &[], Some((0x0a, 1))),
            Some(HostSwitchRequest::Announced { leader_host: 1 }),
        );
    }

    #[test]
    fn an_announcement_is_ignored_when_the_feature_was_never_resolved() {
        assert_eq!(
            decode_request(&announcement_message(), 0xff, 0x09, &[], None),
            None,
            "a keyboard without 0x1814 must not have its other reports mistaken for one"
        );
    }

    #[test]
    fn another_feature_reporting_on_its_own_is_not_an_announcement() {
        let mut raw = ANNOUNCEMENT;
        // 0x0c is Backlight2 on this keyboard, which reports unprompted as the
        // backlight fades. Matching on "some feature sent something" would read
        // that as a host change.
        raw[1] = 0x0c;

        assert_eq!(
            decode_request(
                &v20::Message::from(HidppMessage::Long(raw)),
                0xff,
                0x09,
                &[],
                Some((0x0a, 1)),
            ),
            None,
        );
    }

    #[test]
    fn follows_to_the_one_other_paired_host() {
        assert_eq!(sole_other_host(1, &[0, 1]), Some(0));
        assert_eq!(sole_other_host(0, &[0, 1]), Some(1));
    }

    #[test]
    fn refuses_to_guess_between_two_other_hosts() {
        assert_eq!(sole_other_host(0, &[0, 1, 2]), None);
    }

    #[test]
    fn stays_put_when_no_other_host_is_paired() {
        assert_eq!(sole_other_host(1, &[1]), None);
        assert_eq!(sole_other_host(1, &[]), None);
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

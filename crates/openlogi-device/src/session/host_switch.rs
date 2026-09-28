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
    device::{Device, DeviceError},
    feature::{CreatableFeature, change_host::ChangeHostFeature},
    protocol::v20,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, info};

mod restore;
mod slots;
mod transition;
pub use slots::ReportedHostSlot;
use slots::ReportedHostSlotReader;
pub use transition::switch_linked_hosts;
#[cfg(test)]
use transition::{host_change_required, prepare_host_change_on, shares_channel};

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

/// Whether OpenLogi must command the keyboard or its firmware already began
/// the requested transition after reporting a physical Easy-Switch press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardHostTransition {
    /// Move linked devices first, then command the keyboard last.
    CommandRequired,
    /// An undiverted analytics event was observed. Revalidate and command the
    /// keyboard while it remains reachable. If the destination cannot be
    /// revalidated, linked devices remain on the current host.
    AnalyticsEvent {
        /// Pairing status sampled at the analytics event boundary.
        host_slot: ReportedHostSlot,
    },
    /// The keyboard firmware reported its own departure. An explicitly empty
    /// destination fails closed; otherwise followers move through their own
    /// channels without reopening the departing keyboard.
    AlreadyDeparting {
        /// Pairing status sampled at the departure announcement boundary.
        host_slot: ReportedHostSlot,
    },
}

impl KeyboardHostTransition {
    /// Whether a ChangeHost announcement proved that the keyboard firmware
    /// already began the transition.
    #[must_use]
    pub const fn announcement_observed(self) -> bool {
        matches!(self, Self::AlreadyDeparting { .. })
    }
}

/// How a host-switch session should observe the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSwitchCaptureMode {
    /// Discover and arm the keyboard's reportable host controls.
    Full,
    /// Resolve ChangeHost and listen only for its departure announcement.
    ChangeHostAnnouncement,
}

/// A host-switch request captured from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSwitchRequest {
    /// Zero-based destination host slot.
    pub host: u8,
    /// How the keyboard itself will reach the destination.
    pub keyboard_transition: KeyboardHostTransition,
}
const EASY_SWITCH_HOST_COUNT: u8 = 3;
const EVENT_SLOT_VALIDATION_TIMEOUT: Duration = Duration::from_millis(250);
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

#[derive(Clone)]
struct ChangeHostCapture {
    feature_index: u8,
    slot_reader: ReportedHostSlotReader,
}

impl ChangeHostCapture {
    async fn resolve(feature_index: u8, device: &mut Device) -> Self {
        let slot_reader = ReportedHostSlotReader::resolve(device).await;
        Self {
            feature_index,
            slot_reader,
        }
    }

    async fn reported_host_slot(&self, host: u8) -> ReportedHostSlot {
        if self.slot_reader.is_supported() {
            match timeout(
                EVENT_SLOT_VALIDATION_TIMEOUT,
                self.slot_reader.read_one(host),
            )
            .await
            {
                Ok(ReportedHostSlot::Empty) => ReportedHostSlot::Empty,
                Ok(ReportedHostSlot::Paired) => ReportedHostSlot::Paired,
                Ok(ReportedHostSlot::Unknown) => {
                    debug!(host, "host-slot validation was inconclusive");
                    ReportedHostSlot::Unknown
                }
                Err(_) => {
                    debug!(
                        host,
                        "keyboard did not answer host-slot validation before departure"
                    );
                    ReportedHostSlot::Unknown
                }
            }
        } else {
            ReportedHostSlot::Unknown
        }
    }

    async fn departure_request(&self, host: u8) -> Result<HostSwitchRequest, HostSwitchError> {
        let host_slot = self.reported_host_slot(host).await;
        Ok(HostSwitchRequest {
            host,
            keyboard_transition: KeyboardHostTransition::AlreadyDeparting { host_slot },
        })
    }
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
    /// Opening the addressed HID++ device failed.
    #[error("HID++ device error: {0}")]
    Device(#[from] DeviceError),
    /// A captured analytics event has no freshly verified destination.
    #[error("host {host} pairing could not be verified")]
    HostSlotUnverified {
        /// Zero-based destination slot.
        host: u8,
    },
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

impl HostSwitchError {
    /// Whether the error means the keyboard may already have departed after
    /// reporting an analytics-only host key. Validation failures are not
    /// departure signals: switching targets after one would strand them.
    fn is_device_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Hid(BackendError::Disconnected)
                | Self::KeyboardNotFound
                | Self::Device(DeviceError::DeviceNotFound)
        )
    }
}

impl From<IoSuspended> for HostSwitchError {
    fn from(error: IoSuspended) -> Self {
        Self::Hid(error.into())
    }
}

/// Capture host switch keys until a press, shutdown, or channel retirement.
///
/// Returns any requested host together with the restoration outcome. The caller
/// must retain pending restoration and finish it before starting a successor
/// session. A genuine departure announcement can move followers immediately;
/// restoration then waits for the keyboard to reconnect on a fresh channel.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    registry: &ChannelRegistry,
    capture_mode: HostSwitchCaptureMode,
    device_io: DeviceIoGate,
) -> Result<HostSwitchSessionOutcome, HostSwitchSessionFailure> {
    device_io.ensure_allowed().map_err(HostSwitchError::from)?;
    let shared = registry
        .lookup(&keyboard)
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let channel = Arc::clone(shared.channel());
    let keyboard_index = shared.device_index();
    let mut device = timed_device(
        "opening keyboard device",
        Device::new(Arc::clone(&channel), keyboard_index),
    )
    .await?;
    let change_host = match timed_hidpp(
        "locating change-host announcements",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await
    {
        Ok(Some(info)) => Some(ChangeHostCapture::resolve(info.index, &mut device).await),
        Ok(None) | Err(_) => None,
    };
    if capture_mode == HostSwitchCaptureMode::ChangeHostAnnouncement {
        let capture = change_host.ok_or(HostSwitchError::UnsupportedKeyboard)?;
        return monitor_announcement_session(shutdown, registry, &shared, capture, device_io).await;
    }
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
    let announcement_index = change_host.as_ref().map(|capture| capture.feature_index);
    let listener = channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        if let Some(host) = announcement_index.and_then(|index| {
            change_host_announcement(&message, keyboard_index, index, EASY_SWITCH_HOST_COUNT)
        }) {
            let _ = press_tx.send(HostSwitchRequest {
                host,
                keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
                    host_slot: ReportedHostSlot::Unknown,
                },
            });
        } else if let Some(event) =
            reprog_controls::decode_full_event(&message, keyboard_index, feature_index)
            && let Some(request) = host_control_request(&event_controls, event)
        {
            let _ = press_tx.send(request);
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
    finish_host_switch_session(
        stop,
        registry,
        &shared,
        &controls,
        armed,
        change_host,
        &device_io,
    )
    .await
}

/// Preserve cleanup ownership separately from the captured transition.
async fn finish_host_switch_session(
    stop: HostSwitchStop,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    controls: &ReprogControlsV4,
    armed: Vec<ArmedControl>,
    change_host: Option<ChangeHostCapture>,
    device_io: &DeviceIoGate,
) -> Result<HostSwitchSessionOutcome, HostSwitchSessionFailure> {
    let mut requested_host = stop.requested_host();
    if device_io.allows_io()
        && let Some(request) = &mut requested_host
        && request.keyboard_transition.announcement_observed()
        && let Some(capture) = change_host
    {
        *request = capture.departure_request(request.host).await?;
    }
    let Some(mut pending) = PendingHostSwitchRestore::new(shared, controls.feature_index(), armed)
    else {
        return Ok(HostSwitchSessionOutcome::Restored { requested_host });
    };
    // A departing keyboard cannot restore now. Retain the obligation for its
    // next publication while allowing followers to use their own channels.
    if requested_host.is_some_and(|request| request.keyboard_transition.announcement_observed()) {
        return Ok(HostSwitchSessionOutcome::RestorePending {
            requested_host,
            restore: pending,
        });
    }
    let reuse_armed_channel = match stop {
        // A press does not retire the channel: teardown may write through it
        // for as long as inventory still publishes it.
        HostSwitchStop::Pressed(_) => registry.is_current(shared),
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
    Pressed(HostSwitchRequest),
    /// Teardown was requested while inventory still published the channel
    /// that armed the session.
    Shutdown,
    /// The channel that armed the session must not be written through again:
    /// inventory removed or replaced it, or the owner reported the keyboard
    /// lost.
    ChannelChanged,
}

impl HostSwitchStop {
    fn requested_host(self) -> Option<HostSwitchRequest> {
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
    presses: &mut mpsc::UnboundedReceiver<HostSwitchRequest>,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    mut device_io: DeviceIoGate,
) -> HostSwitchStop {
    let mut registry_changes = registry.subscribe();
    loop {
        // The final announcement can be queued before inventory publishes the
        // departure. Consume that evidence before handling retirement.
        if let Ok(request) = presses.try_recv() {
            return HostSwitchStop::Pressed(request);
        }
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
        let Some(host) = reprog_controls::host_switch_channel(info) else {
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

async fn timed_device<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, DeviceError>>,
) -> Result<T, HostSwitchError> {
    timeout(HIDPP_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| HostSwitchError::TimedOut { operation })?
        .map_err(HostSwitchError::Device)
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

fn hidpp_error(operation: &'static str, error: impl std::fmt::Debug) -> HostSwitchError {
    HostSwitchError::Hidpp(format!("{operation}: {error:?}"))
}

fn event_control(
    controls: &[ArmedControl],
    event: reprog_controls::ReprogControlsEvent,
) -> Option<ArmedControl> {
    match event {
        reprog_controls::ReprogControlsEvent::DivertedButtons(cids) => controls
            .iter()
            .find_map(|control| cids.contains(&control.cid.into()).then_some(*control)),
        reprog_controls::ReprogControlsEvent::AnalyticsKeyEvents(events) => {
            controls.iter().find_map(|control| {
                events
                    .iter()
                    .any(|event| event.cid.0 == control.cid)
                    .then_some(*control)
            })
        }
        reprog_controls::ReprogControlsEvent::DivertedRawMouseXy { .. }
        | reprog_controls::ReprogControlsEvent::DivertedRawWheel { .. } => None,
    }
}

/// Decode a keyboard's `0x1814` host-change announcement.
///
/// Some analytics-only keyboards announce the physical Easy-Switch press on
/// ChangeHost function 0 instead of emitting a ReprogControls analytics event.
fn change_host_announcement(
    message: &v20::Message,
    device_index: u8,
    feature_index: u8,
    host_count: u8,
) -> Option<u8> {
    let header = message.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.function_id.to_lo() != 0
        || header.software_id.to_lo() != 0
    {
        return None;
    }
    let target_host = message.extend_payload()[1];
    (target_host < host_count).then_some(target_host)
}

#[cfg(test)]
mod tests;

fn host_control_request(
    controls: &[ArmedControl],
    event: reprog_controls::ReprogControlsEvent,
) -> Option<HostSwitchRequest> {
    let control = event_control(controls, event)?;
    Some(HostSwitchRequest {
        host: control.host,
        keyboard_transition: match control.mode {
            ReportingMode::Diverted => KeyboardHostTransition::CommandRequired,
            ReportingMode::Analytics => KeyboardHostTransition::AnalyticsEvent {
                host_slot: ReportedHostSlot::Unknown,
            },
        },
    })
}

async fn monitor_announcement_session(
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    capture: ChangeHostCapture,
    device_io: DeviceIoGate,
) -> Result<HostSwitchSessionOutcome, HostSwitchSessionFailure> {
    let (press_tx, mut press_rx) = mpsc::unbounded_channel();
    let index = shared.device_index();
    let feature_index = capture.feature_index;
    let listener = shared
        .channel()
        .add_msg_listener_guarded(move |raw, matched| {
            if !matched
                && let Some(host) = change_host_announcement(
                    &v20::Message::from(raw),
                    index,
                    feature_index,
                    EASY_SWITCH_HOST_COUNT,
                )
            {
                let _ = press_tx.send(HostSwitchRequest {
                    host,
                    keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
                        host_slot: ReportedHostSlot::Unknown,
                    },
                });
            }
        });
    let stop =
        monitor_host_switch(shutdown, &mut press_rx, registry, shared, device_io.clone()).await;
    drop(listener);
    let requested_host = match stop.requested_host() {
        Some(request) if device_io.allows_io() => {
            Some(capture.departure_request(request.host).await?)
        }
        request => request,
    };
    Ok(HostSwitchSessionOutcome::Restored { requested_host })
}

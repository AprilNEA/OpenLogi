//! Keyboard-initiated host-switch synchronization.
//!
//! A session temporarily diverts the keyboard's three host controls, observes
//! which channel was pressed, switches the linked devices, and then
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

use crate::{
    ChannelPool, DeviceIoGate, DeviceRoute,
    backend::BackendError,
    reprog_controls::{self, ReprogControlsV4},
};

mod slots;
mod transition;

pub use slots::ReportedHostSlot;
use slots::ReportedHostSlotReader;
pub use transition::switch_linked_hosts;
#[cfg(test)]
use transition::{host_change_required, prepare_host_change_on, shares_channel};

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
const HIDPP_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
/// A host event can be the keyboard's last message on this host. Bound the
/// event-boundary slot read so follower switching never waits behind the
/// general multi-second HID++ timeout.
const EVENT_SLOT_VALIDATION_TIMEOUT: Duration = Duration::from_millis(250);
/// Analytics-only keyboards can begin leaving as soon as their host key is
/// pressed. Give a reachable keyboard a brief chance to accept cleanup without
/// putting linked-device forwarding behind a multi-second HID++ timeout.
const ANALYTICS_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlCleanup {
    /// The keyboard is expected to remain reachable, so retry every restore.
    Full,
    /// The keyboard may already be leaving, so make one bounded restore pass.
    Prompt,
}

#[derive(Clone, Copy)]
struct CapturedHostSwitch {
    request: HostSwitchRequest,
    cleanup: ControlCleanup,
}

enum CapturedSessionEvent {
    Control(CapturedHostSwitch),
    DepartureAnnouncement { host: u8 },
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

struct HostSwitchFeatures {
    controls: ReprogControlsV4,
    change_host: Option<ChangeHostCapture>,
}

enum SessionOutcome {
    Captured(CapturedSessionEvent),
    Stopped(HostSwitchStopReason),
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
    /// A host event was captured, but its destination slot could not be
    /// verified while the keyboard remained reachable.
    #[error("host {host} pairing could not be verified")]
    HostSlotUnverified {
        /// The zero-based host slot whose pairing could not be verified.
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

/// Capture host switch keys on `keyboard` until one is pressed or `shutdown`
/// resolves. Reachable controls are restored before a requested host is
/// returned; analytics-only capture uses a short cleanup budget because the
/// keyboard may already be leaving.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    channel_pool: ChannelPool,
    capture_mode: HostSwitchCaptureMode,
    mut device_io: DeviceIoGate,
) -> Result<Option<HostSwitchRequest>, HostSwitchError> {
    if !device_io.allows_io() {
        return Err(HostSwitchError::Hid(BackendError::Backend(
            "host device I/O is suspended".into(),
        )));
    }
    let channel = open_channel(&channel_pool, &keyboard, "opening keyboard channel")
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    let keyboard_index = keyboard.device_index();
    if capture_mode == HostSwitchCaptureMode::ChangeHostAnnouncement {
        let change_host = resolve_change_host_capture(&channel, keyboard_index).await?;
        return run_change_host_announcement_session(
            keyboard,
            keyboard_index,
            change_host,
            shutdown,
            channel,
            device_io,
        )
        .await;
    }
    let HostSwitchFeatures {
        controls,
        change_host,
    } = discover_host_switch_features(&channel, keyboard_index).await?;

    let armed = arm_host_controls(&controls).await?;
    if armed.is_empty() {
        return Err(HostSwitchError::UnsupportedKeyboard);
    }

    let (press_tx, mut press_rx) = mpsc::unbounded_channel();
    let feature_index = controls.feature_index();
    let event_controls = armed.clone();
    let announcement_feature_index = change_host.as_ref().map(|capture| capture.feature_index);
    let listener = channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        if let Some(event) =
            reprog_controls::decode_full_event(&message, keyboard_index, feature_index)
        {
            debug!(
                ?event,
                keyboard_index, feature_index, "decoded keyboard control event"
            );
            if let Some(captured) = host_control_request(&event_controls, event) {
                debug!(host = captured.request.host, "matched host control event");
                let _ = press_tx.send(CapturedSessionEvent::Control(captured));
            }
            return;
        }
        if let Some(host) = announcement_feature_index.and_then(|feature_index| {
            change_host_announcement(
                &message,
                keyboard_index,
                feature_index,
                EASY_SWITCH_HOST_COUNT,
            )
        }) {
            debug!(host, "matched change-host announcement");
            let _ = press_tx.send(CapturedSessionEvent::DepartureAnnouncement { host });
        }
    });

    info!(
        route = %keyboard,
        controls = armed.len(),
        "host switch link active"
    );
    let outcome = tokio::select! {
        reason = shutdown => SessionOutcome::Stopped(
            reason.unwrap_or(HostSwitchStopReason::DeviceLost)
        ),
        Some(captured) = press_rx.recv() => SessionOutcome::Captured(captured),
    };

    drop(listener);
    finish_captured_session(
        outcome,
        &controls,
        armed,
        change_host.as_ref(),
        &mut device_io,
    )
    .await
}

async fn finish_captured_session(
    outcome: SessionOutcome,
    controls: &ReprogControlsV4,
    armed: Vec<ArmedControl>,
    change_host: Option<&ChangeHostCapture>,
    device_io: &mut DeviceIoGate,
) -> Result<Option<HostSwitchRequest>, HostSwitchError> {
    let request = match outcome {
        SessionOutcome::Captured(CapturedSessionEvent::Control(captured)) => {
            if !device_io.wait_until_allowed().await {
                return Ok(None);
            }
            Some(finish_captured_control(captured, controls, armed, change_host).await)
        }
        SessionOutcome::Captured(CapturedSessionEvent::DepartureAnnouncement { host }) => {
            if !device_io.wait_until_allowed().await {
                return Ok(None);
            }
            let capture = change_host.ok_or(HostSwitchError::UnsupportedKeyboard)?;
            Some(capture.departure_request(host).await?)
        }
        SessionOutcome::Stopped(HostSwitchStopReason::Graceful) => {
            if device_io.wait_until_allowed().await {
                restore_host_controls(controls, armed).await;
            }
            None
        }
        SessionOutcome::Stopped(HostSwitchStopReason::DeviceLost) => None,
    };
    Ok(request)
}

async fn finish_captured_control(
    captured: CapturedHostSwitch,
    controls: &ReprogControlsV4,
    armed: Vec<ArmedControl>,
    change_host: Option<&ChangeHostCapture>,
) -> HostSwitchRequest {
    let mut request = captured.request;
    if matches!(
        request.keyboard_transition,
        KeyboardHostTransition::AnalyticsEvent { .. }
    ) {
        let host_slot = match change_host {
            Some(capture) => capture.reported_host_slot(request.host).await,
            None => ReportedHostSlot::Unknown,
        };
        request.keyboard_transition = KeyboardHostTransition::AnalyticsEvent { host_slot };
    }
    match captured.cleanup {
        ControlCleanup::Full => restore_host_controls(controls, armed).await,
        ControlCleanup::Prompt => restore_host_controls_promptly(controls, armed).await,
    }
    request
}

async fn resolve_change_host_capture(
    channel: &Arc<HidppChannel>,
    keyboard_index: u8,
) -> Result<ChangeHostCapture, HostSwitchError> {
    let mut device = timed_device(
        "opening keyboard device",
        Device::new(Arc::clone(channel), keyboard_index),
    )
    .await?;
    let feature_index = timed_hidpp(
        "locating change-host announcements",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await?
    .map(|feature| feature.index)
    .ok_or(HostSwitchError::UnsupportedKeyboard)?;
    Ok(ChangeHostCapture::resolve(feature_index, &mut device).await)
}

async fn discover_host_switch_features(
    channel: &Arc<HidppChannel>,
    keyboard_index: u8,
) -> Result<HostSwitchFeatures, HostSwitchError> {
    let mut device = timed_device(
        "opening keyboard device",
        Device::new(Arc::clone(channel), keyboard_index),
    )
    .await?;
    let feature = timed_hidpp(
        "locating host controls",
        device.root().get_feature(reprog_controls::FEATURE_ID),
    )
    .await?
    .ok_or(HostSwitchError::UnsupportedKeyboard)?;
    let controls = ReprogControlsV4::new(Arc::clone(channel), keyboard_index, feature.index);
    let change_host = match timed_hidpp(
        "locating change-host announcements",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await
    {
        Ok(Some(info)) => Some(ChangeHostCapture::resolve(info.index, &mut device).await),
        Ok(None) => None,
        Err(error) => {
            debug!(%error, "change-host announcement lookup failed");
            None
        }
    };
    Ok(HostSwitchFeatures {
        controls,
        change_host,
    })
}

async fn run_change_host_announcement_session(
    keyboard: DeviceRoute,
    keyboard_index: u8,
    change_host: ChangeHostCapture,
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    channel: Arc<HidppChannel>,
    mut device_io: DeviceIoGate,
) -> Result<Option<HostSwitchRequest>, HostSwitchError> {
    let (press_tx, mut press_rx) = mpsc::unbounded_channel();
    let announcement_feature_index = change_host.feature_index;
    let listener = channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        if let Some(host) = change_host_announcement(
            &message,
            keyboard_index,
            announcement_feature_index,
            EASY_SWITCH_HOST_COUNT,
        ) {
            let _ = press_tx.send(host);
        }
    });
    info!(route = %keyboard, "host switch announcement link active");
    let host = tokio::select! {
        _ = shutdown => None,
        host = press_rx.recv() => host,
    };
    drop(listener);
    match host {
        Some(host) if device_io.wait_until_allowed().await => {
            Ok(Some(change_host.departure_request(host).await?))
        }
        Some(_) | None => Ok(None),
    }
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
                    let echo = timed_hidpp(
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
                    debug!(
                        cid = format_args!("{:#06x}", info.cid),
                        ?echo,
                        "analytics reporting enabled"
                    );
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

async fn restore_host_controls_promptly(controls: &ReprogControlsV4, armed: Vec<ArmedControl>) {
    let restore_count = armed.len();
    let restore = async {
        for control in armed {
            if let Err(error) = restore_host_control(controls, control).await {
                debug!(
                    ?error,
                    cid = control.cid,
                    "could not promptly restore host switch control"
                );
            }
        }
    };
    if timeout(ANALYTICS_CLEANUP_TIMEOUT, restore).await.is_err() {
        debug!(
            controls = restore_count,
            "prompt host switch cleanup exceeded its budget"
        );
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

async fn timed_device<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, DeviceError>>,
) -> Result<T, HostSwitchError> {
    timeout(HIDPP_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| HostSwitchError::TimedOut { operation })?
        .map_err(HostSwitchError::Device)
}

fn hidpp_error(operation: &'static str, error: impl std::fmt::Debug) -> HostSwitchError {
    HostSwitchError::Hidpp(format!("{operation}: {error:?}"))
}

fn event_control(
    controls: &[ArmedControl],
    event: &reprog_controls::ReprogControlsEvent,
) -> Option<ArmedControl> {
    match event {
        reprog_controls::ReprogControlsEvent::DivertedButtons(cids) => controls
            .iter()
            .find(|control| cids.contains(&control.cid.into()))
            .copied(),
        reprog_controls::ReprogControlsEvent::AnalyticsKeyEvents(events) => controls
            .iter()
            .find(|control| events.iter().any(|event| event.cid.0 == control.cid))
            .copied(),
        reprog_controls::ReprogControlsEvent::DivertedRawMouseXy { .. }
        | reprog_controls::ReprogControlsEvent::DivertedRawWheel { .. } => None,
    }
}

fn host_control_request(
    controls: &[ArmedControl],
    event: reprog_controls::ReprogControlsEvent,
) -> Option<CapturedHostSwitch> {
    let control = event_control(controls, &event)?;
    let (keyboard_transition, cleanup) = match control.mode {
        ReportingMode::Diverted => (
            KeyboardHostTransition::CommandRequired,
            ControlCleanup::Full,
        ),
        ReportingMode::Analytics => (
            KeyboardHostTransition::AnalyticsEvent {
                host_slot: ReportedHostSlot::Unknown,
            },
            ControlCleanup::Prompt,
        ),
    };
    Some(CapturedHostSwitch {
        request: HostSwitchRequest {
            host: control.host,
            keyboard_transition,
        },
        cleanup,
    })
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

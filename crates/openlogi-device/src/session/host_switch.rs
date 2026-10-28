//! Keyboard-initiated host-switch synchronization.
//!
//! A session watches one keyboard and moves the linked pointing devices to
//! the host that the keyboard switches to. It learns of a switch in one of
//! three ways:
//!
//! - Announcement: a keyboard can announce a switch that the user starts on
//!   it with a `ChangeHost` (`0x1814`) event, before it leaves this host.
//!   Logi Options+ relies on this alone for a keyboard with version 2 or
//!   later that reports
//!   [`ChangeHostFeatureCapabilities::ANNOUNCES_HOST_CHANGE`]
//!   (reverse-engineered, see [`hidpp::feature::change_host`]). The session
//!   listens on every keyboard, because some keyboards without that
//!   capability also announce (a K880, #1145). When the keyboard reports the
//!   capability, the session leaves the reporting of its host keys alone.
//! - Diverted keys: the session diverts the host keys (`0x1b04`), so the
//!   keyboard does not switch by itself. The session moves the linked devices
//!   first and the keyboard last: once the keyboard leaves this host, its
//!   HID++ channel can no longer command a mouse that shares its receiver.
//! - Analytics: the host keys keep their function and also report to
//!   software.
//!
//! After an announcement or an analytics report the keyboard is already
//! leaving. The session then moves the linked devices itself, at once, and
//! does not write to the keyboard again: such a write waits for a keyboard
//! that is gone (#1623).

use std::{future::Future, sync::Arc, time::Duration};

use hidpp::{
    channel::{HidppChannel, MessageListenerGuard},
    device::Device,
    feature::{
        CreatableFeature,
        change_host::{
            ANNOUNCEMENT_MIN_VERSION, ChangeHostFeature, ChangeHostFeatureCapabilities,
            HostChangeRequest, decode_host_change_request,
        },
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

mod cookies;
mod hosts;
mod restore;

pub use cookies::{
    FollowerLink, FollowerRelease, LeadCookie, link_follower_cookies_on,
    release_follower_cookies_on,
};
use hosts::{HostTable, KeyboardHosts, follower_host, read_host_table, renames_host};
use restore::rollback_host_switch_start;
pub use restore::{
    HostSwitchFollowUp, HostSwitchRestoreOutcome, HostSwitchSessionFailure,
    HostSwitchSessionOutcome, PendingHostSwitchRestore,
};

use crate::{
    ChannelPool, ChannelRegistry, DeviceIoGate, DeviceRoute, IoSuspended, SharedChannel,
    backend::BackendError,
    channel::registry::PublicationState,
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// The keyboard has no `ChangeHost`, so it can neither switch hosts nor
    /// announce a switch.
    #[error("keyboard has no host-change feature")]
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

/// Watch `keyboard` for a host switch until a switch, shutdown, or channel
/// retirement, and move `targets` after the keyboard.
///
/// A diverted key press returns the requested host
/// ([`HostSwitchFollowUp::Switch`]): the caller moves `targets` and then the
/// keyboard. When the keyboard switches by itself, the session moves
/// `targets` before it returns ([`HostSwitchFollowUp::AwaitDeparture`]).
/// The caller must retain any pending restoration and finish it before it
/// switches hosts or starts a successor session.
pub async fn run_host_switch_session(
    keyboard: DeviceRoute,
    targets: &[DeviceRoute],
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    registry: &ChannelRegistry,
    channel_pool: &ChannelPool,
    device_io: DeviceIoGate,
) -> Result<HostSwitchSessionOutcome, HostSwitchSessionFailure> {
    device_io.ensure_allowed().map_err(HostSwitchError::from)?;
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
    let change_host = ChangeHostOnKeyboard::locate(&mut device).await?;
    let followers = Followers {
        targets,
        registry,
        channel_pool,
        device_io: &device_io,
    };

    let (trigger_tx, mut triggers) = mpsc::unbounded_channel();
    // Installed before every read of the keyboard's state, so that an
    // announcement made while the session starts is not lost.
    let announcement_listener = listen_for_announcements(
        &channel,
        keyboard_index,
        change_host.index,
        trigger_tx.clone(),
    );
    let position = match KeyboardPosition::read(&change_host).await {
        Ok(position) => position,
        Err(error) => {
            // A keyboard that is leaving stops answering, so the read can
            // fail because of the switch itself.
            let Some(host) = announced_host(&mut triggers) else {
                return Err(error.into());
            };
            debug!(route = %keyboard, %error, "host read failed during an announced switch");
            return Ok(depart(Departure::new(&shared, None, host), None, followers).await);
        }
    };
    let mut hosts = KeyboardHosts::locate(&mut device).await;
    // Installed before the table is read, so that no rename is missed.
    let rename_listener = hosts
        .feature_index()
        .map(|index| listen_for_renames(&channel, keyboard_index, index, trigger_tx.clone()));
    hosts.read().await;

    let (armed, controls_index) =
        match arm_host_keys(&mut device, &shared, position.announces).await {
            Ok(armed) => armed,
            Err(failure) => {
                let (error, pending) = *failure;
                // A keyboard that is leaving stops answering, so the arm can
                // fail because of the switch itself. The linked devices still
                // follow a departure that the keyboard queued.
                if let Some(HostSwitchStop::Departed(host)) =
                    queued_departure(&mut triggers, position, &mut hosts)
                {
                    debug!(route = %keyboard, %error, "host keys failed to arm during a departure");
                    let departure = Departure::new(&shared, hosts.table(), host);
                    return Ok(depart(departure, pending, followers).await);
                }
                return Err(rollback_host_switch_start(error, pending, registry, &device_io).await);
            }
        };

    let key_listener = controls_index.filter(|_| !armed.is_empty()).map(|index| {
        listen_for_host_keys(
            &channel,
            keyboard_index,
            index,
            armed.clone(),
            trigger_tx.clone(),
        )
    });
    drop(trigger_tx);

    info!(
        route = %keyboard,
        host = position.current_host,
        change_host_version = position.version,
        announces = position.announces,
        controls = armed.len(),
        "host switch link active"
    );
    let stop = monitor_host_switch(
        shutdown,
        &mut triggers,
        position,
        registry,
        &shared,
        device_io.clone(),
        &mut hosts,
    )
    .await;
    drop(key_listener);
    drop(rename_listener);
    drop(announcement_listener);

    let pending =
        controls_index.and_then(|index| PendingHostSwitchRestore::new(&shared, index, armed));
    if let HostSwitchStop::Departed(host) = stop {
        let departure = Departure::new(&shared, hosts.table(), host);
        return Ok(depart(departure, pending, followers).await);
    }

    Ok(restore_after(stop, pending, &shared, registry, &device_io).await)
}

/// The host keys that a session armed, and the `ReprogControlsV4` index
/// they were armed through.
type ArmedHostKeys = (Vec<ArmedControl>, Option<u8>);

/// Why the host keys could not be armed, and the restoration that a partial
/// arm left.
type ArmFailure = (HostSwitchError, Option<PendingHostSwitchRestore>);

/// Arm the host keys of the keyboard on `shared`, unless it `announces` its
/// host changes. A keyboard without `ReprogControlsV4` arms nothing.
async fn arm_host_keys(
    device: &mut Device,
    shared: &SharedChannel,
    announces: bool,
) -> Result<ArmedHostKeys, Box<ArmFailure>> {
    let mut armed = Vec::new();
    if announces {
        return Ok((armed, None));
    }
    let feature = match timed_hidpp(
        "locating host controls",
        device.root().get_feature(reprog_controls::FEATURE_ID),
    )
    .await
    {
        Ok(Some(feature)) => feature,
        Ok(None) => return Ok((armed, None)),
        Err(error) => return Err(Box::new((error, None))),
    };
    let controls = ReprogControlsV4::new(
        Arc::clone(shared.channel()),
        shared.device_index(),
        feature.index,
    );
    match arm_host_controls_inner(&controls, &mut armed).await {
        Ok(()) => Ok((armed, Some(feature.index))),
        Err(error) => {
            let pending = PendingHostSwitchRestore::new(shared, feature.index, armed);
            Err(Box::new((error, pending)))
        }
    }
}

/// The devices that a session moves after its keyboard, and what moving them
/// needs.
#[derive(Clone, Copy)]
struct Followers<'a> {
    targets: &'a [DeviceRoute],
    registry: &'a ChannelRegistry,
    channel_pool: &'a ChannelPool,
    device_io: &'a DeviceIoGate,
}

/// Move the `followers` after a keyboard that is leaving this host, and
/// return the restoration of its host keys, which the owner finishes once the
/// keyboard is back.
async fn depart(
    departure: Departure<'_>,
    pending: Option<PendingHostSwitchRestore>,
    followers: Followers<'_>,
) -> HostSwitchSessionOutcome {
    let Followers {
        targets,
        registry,
        channel_pool,
        device_io,
    } = followers;
    let shared = departure.keyboard;
    info!(
        route = %shared.route(),
        host = departure.host,
        "keyboard is switching hosts; moving linked devices"
    );
    follow_departure(departure, targets, registry, channel_pool, device_io).await;
    // A restore write now would wait for a keyboard that is leaving. The
    // owner retries it once inventory publishes the keyboard again.
    let restore = pending.map(|pending| {
        if matches!(
            HostSwitchStop::for_current_publication(registry, shared),
            HostSwitchStop::ChannelChanged
        ) {
            pending
        } else {
            pending.allow_current_channel()
        }
    });
    HostSwitchSessionOutcome::Departed { restore }
}

/// Listen on `channel` for the `ChangeHost` announcements of the keyboard at
/// `keyboard_index`, and send each one to `triggers`.
fn listen_for_announcements(
    channel: &HidppChannel,
    keyboard_index: u8,
    feature_index: u8,
    triggers: mpsc::UnboundedSender<HostTrigger>,
) -> MessageListenerGuard {
    channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        if let Some(request) = decode_host_change_request(&message, keyboard_index, feature_index) {
            let _ = triggers.send(HostTrigger::Announced(request));
        }
    })
}

/// Listen on `channel` for the armed host keys of the keyboard at
/// `keyboard_index`, and send each press to `triggers`.
fn listen_for_host_keys(
    channel: &HidppChannel,
    keyboard_index: u8,
    feature_index: u8,
    controls: Vec<ArmedControl>,
    triggers: mpsc::UnboundedSender<HostTrigger>,
) -> MessageListenerGuard {
    channel.add_msg_listener_guarded(move |raw, matched| {
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        let Some(event) =
            reprog_controls::decode_full_event(&message, keyboard_index, feature_index)
        else {
            return;
        };
        if let Some(trigger) = key_trigger(&controls, event) {
            let _ = triggers.send(trigger);
        }
    })
}

/// Listen on `channel` for the replies to renames of the slots of the
/// keyboard at `keyboard_index`, and send a [`HostTrigger::Renamed`] for each.
fn listen_for_renames(
    channel: &HidppChannel,
    keyboard_index: u8,
    feature_index: u8,
    triggers: mpsc::UnboundedSender<HostTrigger>,
) -> MessageListenerGuard {
    channel.add_msg_listener_guarded(move |raw, _| {
        let message = v20::Message::from(raw);
        if renames_host(&message, keyboard_index, feature_index) {
            let _ = triggers.send(HostTrigger::Renamed);
        }
    })
}

/// Restore the host keys that the session armed on `shared`, after a `stop`
/// other than a departure.
async fn restore_after(
    stop: HostSwitchStop,
    pending: Option<PendingHostSwitchRestore>,
    shared: &SharedChannel,
    registry: &ChannelRegistry,
    device_io: &DeviceIoGate,
) -> HostSwitchSessionOutcome {
    let requested_host = stop.requested_host();
    let Some(mut pending) = pending else {
        return HostSwitchSessionOutcome::Restored { requested_host };
    };
    let reuse_armed_channel = match stop {
        // A press does not retire the channel: teardown may write through it
        // for as long as inventory still publishes it.
        HostSwitchStop::Pressed(_) => !matches!(
            HostSwitchStop::for_current_publication(registry, shared),
            HostSwitchStop::ChannelChanged
        ),
        // A withdrawn receiver slot may return on the same USB connection.
        // The token still requires a live exact-route publication before writes.
        HostSwitchStop::Shutdown | HostSwitchStop::ReceiverSlotGone => true,
        HostSwitchStop::ChannelChanged | HostSwitchStop::Departed(_) => false,
    };
    if reuse_armed_channel {
        pending = pending.allow_current_channel();
    }
    if !device_io.allows_io() {
        return HostSwitchSessionOutcome::RestorePending {
            requested_host,
            restore: pending,
        };
    }
    match pending.retry(registry).await {
        HostSwitchRestoreOutcome::Restored => HostSwitchSessionOutcome::Restored { requested_host },
        HostSwitchRestoreOutcome::RestorePending(restore) => {
            HostSwitchSessionOutcome::RestorePending {
                requested_host,
                restore,
            }
        }
    }
}

/// What a listener saw on the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostTrigger {
    /// A diverted host key for this zero-based host was pressed. The keyboard
    /// stays on this host until the session moves it.
    Pressed(u8),
    /// A host key with analytics reporting was pressed for this zero-based
    /// host. The keyboard switches by itself.
    Reported(u8),
    /// The keyboard announced a host change.
    Announced(HostChangeRequest),
    /// A slot of the keyboard was renamed. The keyboard stays on this host.
    Renamed,
}

/// The keyboard's `ChangeHost` state when the session armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyboardPosition {
    feature_index: u8,
    version: u8,
    host_count: u8,
    current_host: u8,
    /// The keyboard reports
    /// [`ChangeHostFeatureCapabilities::ANNOUNCES_HOST_CHANGE`].
    announces: bool,
}

/// `ChangeHost` on the keyboard, located before its state is read.
struct ChangeHostOnKeyboard {
    index: u8,
    version: u8,
    feature: Arc<ChangeHostFeature>,
}

impl ChangeHostOnKeyboard {
    async fn locate(device: &mut Device) -> Result<Self, HostSwitchError> {
        let info = timed_hidpp(
            "locating host-change feature",
            device.root().get_feature(ChangeHostFeature::ID),
        )
        .await?
        .ok_or(HostSwitchError::UnsupportedKeyboard)?;
        Ok(Self {
            index: info.index,
            version: info.version,
            feature: device.add_feature::<ChangeHostFeature>(info.index),
        })
    }
}

impl KeyboardPosition {
    async fn read(change_host: &ChangeHostOnKeyboard) -> Result<Self, HostSwitchError> {
        let feature = &change_host.feature;
        let state = timed_hidpp("reading current host", feature.get_host_info()).await?;
        let announces = change_host.version >= ANNOUNCEMENT_MIN_VERSION
            && match timed_hidpp(
                "reading host-change capabilities",
                feature.get_capabilities(),
            )
            .await
            {
                Ok(capabilities) => {
                    capabilities.contains(ChangeHostFeatureCapabilities::ANNOUNCES_HOST_CHANGE)
                }
                Err(error) => {
                    debug!(%error, "host-change capabilities are unreadable");
                    false
                }
            };
        Ok(Self {
            feature_index: change_host.index,
            version: change_host.version,
            host_count: state.host_count,
            current_host: state.current_host,
            announces,
        })
    }

    /// The stop that `trigger` asks for, or `None` for a trigger that does
    /// not move the keyboard to another valid host.
    fn stop_for(self, trigger: HostTrigger) -> Option<HostSwitchStop> {
        let leaves_for = |host: u8| host < self.host_count && host != self.current_host;
        match trigger {
            HostTrigger::Pressed(host) => Some(HostSwitchStop::Pressed(host)),
            HostTrigger::Reported(host) => {
                leaves_for(host).then_some(HostSwitchStop::Departed(host))
            }
            // The source host must be this one. A payload whose bytes came in
            // the other order would otherwise move the mice to this host.
            HostTrigger::Announced(HostChangeRequest { from, to }) => (from == self.current_host
                && leaves_for(to))
            .then_some(HostSwitchStop::Departed(to)),
            HostTrigger::Renamed => None,
        }
    }
}

/// Why monitoring an armed host-switch session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostSwitchStop {
    /// The keyboard asked for this zero-based host.
    Pressed(u8),
    /// The keyboard is switching to this zero-based host by itself.
    Departed(u8),
    /// Teardown was requested while inventory still published the channel
    /// that armed the session.
    Shutdown,
    /// The keyboard's receiver slot went offline, but inventory still owns
    /// the USB transport. Restoration may reuse it once the slot returns.
    ReceiverSlotGone,
    /// The channel that armed the session must not be written through again:
    /// inventory removed or replaced it, or the owner reported the keyboard
    /// lost.
    ChannelChanged,
}

impl HostSwitchStop {
    fn requested_host(self) -> Option<u8> {
        match self {
            Self::Pressed(host) => Some(host),
            Self::Departed(_) | Self::Shutdown | Self::ReceiverSlotGone | Self::ChannelChanged => {
                None
            }
        }
    }

    /// A stop that did not itself retire the channel. Re-checking inventory
    /// keeps a simultaneously ready replacement from being written underneath.
    fn for_current_publication(registry: &ChannelRegistry, shared: &SharedChannel) -> Self {
        match registry.publication(shared) {
            PublicationState::Current => Self::Shutdown,
            PublicationState::SlotOffline => Self::ReceiverSlotGone,
            PublicationState::Retired => Self::ChannelChanged,
        }
    }

    /// Losing a receiver slot does not retire its still-published transport.
    /// A lost owner on an otherwise current route remains conservative.
    fn for_lost_publication(registry: &ChannelRegistry, shared: &SharedChannel) -> Self {
        match registry.publication(shared) {
            PublicationState::SlotOffline => Self::ReceiverSlotGone,
            PublicationState::Current | PublicationState::Retired => Self::ChannelChanged,
        }
    }
}

async fn monitor_host_switch(
    shutdown: oneshot::Receiver<HostSwitchStopReason>,
    triggers: &mut mpsc::UnboundedReceiver<HostTrigger>,
    position: KeyboardPosition,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    device_io: DeviceIoGate,
    hosts: &mut KeyboardHosts,
) -> HostSwitchStop {
    let stop = watch_host_switch(
        shutdown, triggers, position, registry, shared, device_io, hosts,
    )
    .await;
    if matches!(
        stop,
        HostSwitchStop::ChannelChanged | HostSwitchStop::ReceiverSlotGone
    ) {
        queued_departure(triggers, position, hosts).unwrap_or(stop)
    } else {
        stop
    }
}

/// A departure that a listener queued before the channel changed. The
/// keyboard announces or reports a switch before it leaves, so inventory can
/// retire its channel before the session reads the trigger. The linked
/// devices still follow; a press is dropped, because only the keyboard's
/// channel could move the keyboard.
///
/// A rename queued before the departure makes `hosts` old. The keyboard is
/// leaving and cannot be read again, so the table is dropped, and the linked
/// devices keep their slot numbers.
fn queued_departure(
    triggers: &mut mpsc::UnboundedReceiver<HostTrigger>,
    position: KeyboardPosition,
    hosts: &mut KeyboardHosts,
) -> Option<HostSwitchStop> {
    while let Ok(trigger) = triggers.try_recv() {
        if trigger == HostTrigger::Renamed {
            hosts.forget();
            continue;
        }
        if let Some(stop @ HostSwitchStop::Departed(_)) = position.stop_for(trigger) {
            return Some(stop);
        }
    }
    None
}

/// The host that a queued announcement names, for a session that could not
/// read the keyboard's position. Only a change to another host is checked
/// here. Each linked device checks the host against its own host count and
/// empty slots before it moves.
fn announced_host(triggers: &mut mpsc::UnboundedReceiver<HostTrigger>) -> Option<u8> {
    while let Ok(trigger) = triggers.try_recv() {
        if let HostTrigger::Announced(HostChangeRequest { from, to }) = trigger
            && from != to
        {
            return Some(to);
        }
    }
    None
}

async fn watch_host_switch(
    mut shutdown: oneshot::Receiver<HostSwitchStopReason>,
    triggers: &mut mpsc::UnboundedReceiver<HostTrigger>,
    position: KeyboardPosition,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    mut device_io: DeviceIoGate,
    hosts: &mut KeyboardHosts,
) -> HostSwitchStop {
    let mut registry_changes = registry.subscribe();
    loop {
        if !registry.is_current(shared) {
            info!(route = %shared.route(), "inventory replaced or removed host-switch channel");
            return HostSwitchStop::for_current_publication(registry, shared);
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
                    HostSwitchStopReason::DeviceLost => {
                        HostSwitchStop::for_lost_publication(registry, shared)
                    }
                };
            }
            trigger = triggers.recv() => {
                let Some(trigger) = trigger else {
                    return HostSwitchStop::for_current_publication(registry, shared);
                };
                if trigger == HostTrigger::Renamed {
                    // The table names the slots that a departure maps by name.
                    if device_io.allows_io() {
                        hosts.read().await;
                    } else {
                        hosts.forget();
                    }
                    continue;
                }
                if let Some(stop) = position.stop_for(trigger) {
                    return stop;
                }
                debug!(
                    route = %shared.route(),
                    ?trigger,
                    host = position.current_host,
                    "host switch trigger names no other host"
                );
            }
            allowed = device_io.changed() => {
                if allowed.is_none() {
                    return HostSwitchStop::for_current_publication(registry, shared);
                }
            }
        }
    }
}

/// A keyboard that is switching to `host` by itself.
#[derive(Clone, Copy)]
struct Departure<'a> {
    keyboard: &'a SharedChannel,
    /// The keyboard's slot table, read when the session armed and again
    /// after each rename.
    hosts: Option<&'a HostTable>,
    host: u8,
}

impl<'a> Departure<'a> {
    fn new(keyboard: &'a SharedChannel, hosts: Option<&'a HostTable>, host: u8) -> Self {
        Self {
            keyboard,
            hosts,
            host,
        }
    }
}

/// Move every reachable linked device after a keyboard that is leaving.
/// Never writes to the keyboard. Failures are logged per device.
async fn follow_departure(
    departure: Departure<'_>,
    targets: &[DeviceRoute],
    registry: &ChannelRegistry,
    channel_pool: &ChannelPool,
    device_io: &DeviceIoGate,
) {
    let Departure {
        keyboard,
        hosts,
        host,
    } = departure;
    // The user may be pairing the keyboard to a new computer on an empty
    // slot. A mouse slot with the same number belongs to another computer.
    if hosts.is_some_and(|hosts| hosts.is_empty_slot(host)) {
        info!(route = %keyboard.route(), host, "keyboard switched to an empty slot; linked devices stay");
        return;
    }
    if device_io.ensure_allowed().is_err() {
        info!(route = %keyboard.route(), host, "device I/O is suspended; linked devices stay");
        return;
    }
    for target in targets {
        match follow_on(target, departure, registry, channel_pool).await {
            Ok(Some(slot)) => {
                info!(route = %target, host = slot, "linked device followed the keyboard");
            }
            Ok(None) => {
                debug!(route = %target, host, "linked device already uses the keyboard's host");
            }
            Err(error) => {
                info!(%error, route = %target, host, "linked device could not follow the keyboard");
            }
        }
    }
}

/// Move `target` after the keyboard of `departure`. Returns the slot that it
/// switched to, or `None` when it already used that slot.
async fn follow_on(
    target: &DeviceRoute,
    departure: Departure<'_>,
    registry: &ChannelRegistry,
    channel_pool: &ChannelPool,
) -> Result<Option<u8>, HostSwitchError> {
    let channel = follower_channel(target, departure.keyboard, registry, channel_pool).await?;
    let (slot, change) = prepare_follower_change(
        channel,
        target.device_index(),
        departure.hosts,
        departure.host,
    )
    .await?;
    Ok(apply_host_change(change).await?.then_some(slot))
}

/// Read-only checks before the linked device at `device_index` follows a
/// keyboard to `host`. The slot comes from [`follower_host`]; the device's
/// own slot names are read only when the keyboard names `host`. Returns the
/// slot with the change.
async fn prepare_follower_change(
    channel: Arc<HidppChannel>,
    device_index: u8,
    keyboard_hosts: Option<&HostTable>,
    host: u8,
) -> Result<(u8, PreparedHostChange), HostSwitchError> {
    let mut device =
        timed_hidpp("opening linked device", Device::new(channel, device_index)).await?;
    let named = keyboard_hosts.is_some_and(|hosts| hosts.name(host).is_some());
    let follower = if named {
        read_host_table(&mut device).await
    } else {
        None
    };
    let slot = follower_host(keyboard_hosts, follower.as_ref(), host);
    let change = prepare_host_change_with(&mut device, device_index, slot).await?;
    Ok((slot, change))
}

/// The channel to command `target` through: the one inventory publishes for
/// it, else the keyboard's receiver channel, else a pooled one.
async fn follower_channel(
    target: &DeviceRoute,
    keyboard: &SharedChannel,
    registry: &ChannelRegistry,
    channel_pool: &ChannelPool,
) -> Result<Arc<HidppChannel>, HostSwitchError> {
    if let Some(shared) = registry.lookup(target) {
        return Ok(Arc::clone(shared.channel()));
    }
    if shares_channel(target, keyboard.route()) {
        return Ok(Arc::clone(keyboard.channel()));
    }
    open_channel(channel_pool, target, "opening linked device channel")
        .await?
        .ok_or(HostSwitchError::TargetNotFound)
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
    let mut keyboard_device = timed_hidpp(
        "opening host-change device",
        Device::new(Arc::clone(&channel), keyboard.device_index()),
    )
    .await?;
    let keyboard_change =
        prepare_host_change_with(&mut keyboard_device, keyboard.device_index(), host).await?;
    let keyboard_hosts = read_host_table(&mut keyboard_device).await;
    for target in targets {
        let prepared = prepare_host_change(
            target,
            keyboard_hosts.as_ref(),
            host,
            keyboard,
            &channel,
            channel_pool,
        )
        .await;
        match prepared {
            Ok((slot, change)) => {
                if let Err(error) = apply_host_change(change).await {
                    debug!(%error, route = %target, host = slot, "linked device host switch failed");
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
    keyboard_hosts: Option<&HostTable>,
    host: u8,
    keyboard: &DeviceRoute,
    keyboard_channel: &Arc<HidppChannel>,
    channel_pool: &ChannelPool,
) -> Result<(u8, PreparedHostChange), HostSwitchError> {
    let channel = if shares_channel(target, keyboard) {
        Arc::clone(keyboard_channel)
    } else {
        open_channel(channel_pool, target, "opening linked device channel")
            .await?
            .ok_or(HostSwitchError::TargetNotFound)?
    };
    prepare_follower_change(channel, target.device_index(), keyboard_hosts, host).await
}

/// Read-only checks before `device` moves to `host`: the host is in range,
/// and the device does not report it as an empty slot.
async fn prepare_host_change_with(
    device: &mut Device,
    device_index: u8,
    host: u8,
) -> Result<PreparedHostChange, HostSwitchError> {
    let info = timed_hidpp(
        "locating host-change feature",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await?
    .ok_or_else(|| HostSwitchError::Hidpp("ChangeHost is unsupported".into()))?;
    let change_host = device.add_feature::<ChangeHostFeature>(info.index);
    let state = timed_hidpp("reading current host", change_host.get_host_info()).await?;
    let required = host_change_required(state.current_host, state.host_count, host)?;
    if required && host_slot_is_empty(device, host).await {
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
///
/// Some devices (confirmed on an MX Vertical, see #1709) answer `status ==
/// Empty` for every slot, including the one they are currently connected on
/// and others that are genuinely paired and carry a real friendly name — the
/// status byte is simply unimplemented/stuck at 0 on these units. A nonzero
/// `name_len` is independent evidence the slot has a stored pairing, so it
/// overrides an `Empty` status; only a slot with both `Empty` and no name is
/// treated as truly empty.
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
        Ok(slot) => slot.status == HostSlotStatus::Empty && slot.name_len == 0,
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

/// The trigger that `event` is for one of the armed `controls`. A diverted
/// press and an analytics report count only for a control armed in that
/// mode: the keyboard switches by itself only after the analytics report.
fn key_trigger(
    controls: &[ArmedControl],
    event: reprog_controls::ReprogControlsEvent,
) -> Option<HostTrigger> {
    let armed_as = |mode| controls.iter().filter(move |control| control.mode == mode);
    match event {
        reprog_controls::ReprogControlsEvent::DivertedButtons(cids) => {
            armed_as(ReportingMode::Diverted).find_map(|control| {
                cids.contains(&control.cid.into())
                    .then_some(HostTrigger::Pressed(control.host))
            })
        }
        reprog_controls::ReprogControlsEvent::AnalyticsKeyEvents(events) => {
            armed_as(ReportingMode::Analytics).find_map(|control| {
                events
                    .iter()
                    .any(|event| event.cid.0 == control.cid)
                    .then_some(HostTrigger::Reported(control.host))
            })
        }
        reprog_controls::ReprogControlsEvent::DivertedRawMouseXy { .. }
        | reprog_controls::ReprogControlsEvent::DivertedRawWheel { .. }
        | reprog_controls::ReprogControlsEvent::ReportingChangedElsewhere(_) => None,
    }
}

#[cfg(test)]
mod tests;

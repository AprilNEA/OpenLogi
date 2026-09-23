//! The channel lifecycle every capture session shares.
//!
//! A capture session arms a device-specific set of controls and then does the
//! same thing whatever those controls are: publish its channel for hardware
//! writes to reuse, listen for diverted reports, re-arm when the device
//! reconnects, stop when asked or when inventory replaces the channel, and
//! hand every control back before it stops listening. [`run_capture`] is that
//! sequence. [`ArmedCapture`] is what differs between the gesture and the
//! keyboard session: which controls are armed and what their reports mean.

mod liveness;

use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        root::RootFeature,
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use liveness::{CaptureLiveness, ChannelActivity, LivenessDecision, PingOutcome};

use super::capture_restore::{
    CaptureChannelSlot, CaptureError, CaptureSessionOutcome, CaptureStop, PendingCaptureRestore,
    drop_listener_after, restore_after_stop, stop_for_current_publication, wait_for_channel_change,
};
use super::gesture::CapturedInput;
use crate::{ChannelRegistry, DeviceIoGate, DeviceRoute, SharedChannel};

/// How long a reconnected device gets before its diversions are re-issued.
/// The reconnect broadcast arrives the instant the link is back, occasionally
/// before the device accepts feature writes again.
const REARM_SETTLE_DELAY: Duration = Duration::from_millis(200);

/// What the process running a capture session hands it: where inputs go, when
/// to stop, and the handles that tie the session to the rest of the device
/// layer.
pub struct CaptureHost<'a> {
    /// Receives every [`CapturedInput`] the session decodes.
    pub sink: mpsc::UnboundedSender<CapturedInput>,
    /// Resolves, or is dropped, when the session should restore its controls
    /// and return.
    pub shutdown: oneshot::Receiver<()>,
    /// Where the session publishes its open channel so bounded hardware
    /// writes reuse it instead of opening a second connection.
    pub channel_slot: CaptureChannelSlot,
    /// Inventory's channel publications: the session runs on the one current
    /// for its route and stops when that publication is replaced or removed.
    pub registry: &'a ChannelRegistry,
    /// Host device-I/O gate: the session refuses to start while it is closed
    /// and sends nothing until it reopens.
    pub device_io: DeviceIoGate,
}

impl CaptureHost<'_> {
    /// The channel inventory currently publishes for `route`, once host
    /// device I/O is allowed. A registry miss is not retried here: the caller
    /// runs the session again after a later inventory publication.
    pub(super) fn channel_for(&self, route: &DeviceRoute) -> Result<SharedChannel, CaptureError> {
        let shared = self
            .registry
            .lookup(route)
            .ok_or(CaptureError::DeviceNotFound)?;
        self.device_io.ensure_allowed()?;
        Ok(shared)
    }
}

/// Prove the device behind `shared` answers HID++ before arming anything on
/// it.
pub(super) async fn open_device(shared: &SharedChannel) -> Result<Device, CaptureError> {
    let device_index = shared.device_index();
    Device::new(Arc::clone(shared.channel()), device_index)
        .await
        .map_err(|_| CaptureError::DeviceUnreachable(device_index))
}

/// Whether a session probes a channel that has gone quiet.
///
/// A session's channel is the sole delivery path for every control it
/// diverts, and a channel whose input-report delivery dies (observed on macOS
/// with concurrent opens of one node: writes accepted, replies and events
/// silently routed elsewhere) turns every captured control to dead air with
/// nothing to notice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Liveness {
    /// Ping the device through the session's channel once it has been wholly
    /// idle. Consecutive all-silent pings mean the channel, not the device, is
    /// gone: a sleeping or unreachable device can still send an HID++ error
    /// reply, which proves delivery and resets the count. A transport or
    /// setup error proves neither delivery nor silence, so it stops the
    /// session at once. Stopping lets the manager re-arm on a fresh channel.
    Watched,
    /// Never probe: only inventory replacing the channel ends the session.
    Unwatched,
}

/// The device-specific half of a capture session: the controls one session
/// armed, what their reports mean, and how they are re-armed and handed back.
/// [`run_capture`] owns the rest.
pub(super) trait ArmedCapture {
    /// What this capture is called in the session's logs.
    const NAME: &'static str;
    /// Whether the session probes its channel once it goes quiet.
    const LIVENESS: Liveness;

    /// Log what was armed, once capture is live.
    fn log_active(&self, device_index: u8, wake_rearm: bool);

    /// Build the handler for every unsolicited report on the session's
    /// channel. It runs on the channel's read thread, by shared reference.
    fn report_handler(
        &self,
        device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&v20::Message) + Send + Sync + 'static;

    /// Forget input state that a device power-cycle invalidated.
    fn reset_input_state(&self) {}

    /// Re-issue every diversion after a wireless reconnect. Failures are
    /// logged, not propagated: the next reconnection broadcast retries.
    async fn rearm(&self);

    /// Convert all armed firmware state into the one capability that can
    /// release it. Consuming `self` prevents a session and a restore retry
    /// from both claiming ownership at once.
    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore>;
}

/// Run one armed capture on `shared` until it is told to stop or inventory
/// replaces the channel, then hand every diverted control back.
///
/// A normal stop restores every control before returning; transport
/// replacement or loss may return [`CaptureSessionOutcome::RestorePending`]
/// for the caller to retry on the current inventory channel.
pub(super) async fn run_capture<A: ArmedCapture>(
    shared: SharedChannel,
    armed: A,
    host: CaptureHost<'_>,
) -> CaptureSessionOutcome {
    let CaptureHost {
        sink,
        shutdown,
        channel_slot,
        registry,
        device_io,
    } = host;
    let chan = Arc::clone(shared.channel());
    let device_index = shared.device_index();

    // Publish this device's open channel so hardware writes (DPI, SmartShift,
    // Fn-lock) reuse it instead of opening the same HID node a second time.
    // Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(shared.clone());
    }

    let activity = match A::LIVENESS {
        Liveness::Watched => Some(Arc::new(ChannelActivity::default())),
        Liveness::Unwatched => None,
    };
    let listener = chan.add_msg_listener_guarded({
        let activity = activity.clone();
        let on_report = armed.report_handler(device_index, sink.clone());
        move |raw, matched| {
            // Every parsed inbound HID++ report proves this channel's read
            // path is alive, including responses matched to another request.
            if let Some(activity) = &activity {
                activity.record();
            }
            if matched {
                return;
            }
            on_report(&v20::Message::from(raw));
        }
    });

    // Wireless devices drop their diverted-control state when they
    // power-cycle (idle sleep, power switch, Easy-Switch host change) — the
    // reconnection broadcast on `0x1d4b` is the firmware asking the host to
    // reconfigure. Re-arm on every broadcast, or the captured controls
    // silently revert to their native functions after the first nap.
    let root = RootFeature::new(Arc::clone(&chan), device_index, 0);
    let wireless = root
        .get_feature(WirelessDeviceStatusFeature::ID)
        .await
        .ok()
        .flatten()
        .map(|info| WirelessDeviceStatusFeature::new(Arc::clone(&chan), device_index, info.index));
    armed.log_active(device_index, wireless.is_some());
    let stop = monitor(
        CaptureMonitor {
            armed: &armed,
            root: &root,
            device_index,
            registry,
            shared: &shared,
            activity: activity.as_deref(),
        },
        wireless,
        shutdown,
        device_io,
    )
    .await;

    // The slot is one last-writer-wins cell shared by every session, so a
    // sibling may have published its own channel after ours. Clear it only
    // while it still holds *this* session's channel — evicting the sibling's
    // would silently demote its hardware writes to the fresh-open slow path.
    if let Ok(mut slot) = channel_slot.write()
        && slot
            .as_ref()
            .is_some_and(|published| Arc::ptr_eq(published.channel(), &chan))
    {
        *slot = None;
    }
    // Keep accepting reports until firmware restoration is complete. The
    // agent drains this listener's forwarding task before publishing ordered
    // Done, so this session remains the sole owner of every input captured
    // while its controls could still be diverted.
    let pending = armed.into_pending(&shared);
    let outcome = drop_listener_after(listener, restore_after_stop(stop, pending, registry)).await;
    debug!(index = device_index, capture = A::NAME, "capture stopped");
    outcome
}

/// Borrowed state used while monitoring one armed capture session.
struct CaptureMonitor<'a, A> {
    armed: &'a A,
    root: &'a RootFeature,
    device_index: u8,
    registry: &'a ChannelRegistry,
    shared: &'a SharedChannel,
    /// What the listener records, on a [`Liveness::Watched`] session.
    activity: Option<&'a ChannelActivity>,
}

/// The idle watchdog of one watched session: what its listener records, and
/// the deadline and strike state derived from it.
struct Watchdog<'a> {
    activity: &'a ChannelActivity,
    liveness: CaptureLiveness,
}

impl<'a> Watchdog<'a> {
    fn new(activity: &'a ChannelActivity) -> Self {
        Self {
            activity,
            liveness: CaptureLiveness::new(Instant::now(), activity.seq()),
        }
    }

    /// Start a full idle interval over and clear any strike.
    fn restart(&mut self) {
        self.liveness
            .record_activity(Instant::now(), self.activity.seq());
    }
}

/// The next report on a watched channel. Never resolves on an unwatched one.
async fn next_activity(watchdog: Option<&Watchdog<'_>>) -> u64 {
    match watchdog {
        Some(watchdog) => {
            watchdog
                .activity
                .changed_after(watchdog.liveness.activity_seq())
                .await
        }
        None => std::future::pending().await,
    }
}

/// When a watched channel has been idle for a full interval. Never resolves
/// on an unwatched one.
async fn idle_deadline(watchdog: Option<&Watchdog<'_>>) {
    match watchdog {
        Some(watchdog) => tokio::time::sleep_until(watchdog.liveness.idle_deadline()).await,
        None => std::future::pending().await,
    }
}

/// Keep a capture session alive and reapply its volatile diversions whenever
/// the device announces a reconnect. Returns only the typed reason capture
/// stopped; restoration performs a fresh registry lookup after monitoring.
async fn monitor<A: ArmedCapture>(
    context: CaptureMonitor<'_, A>,
    wireless: Option<WirelessDeviceStatusFeature>,
    shutdown: oneshot::Receiver<()>,
    mut device_io: DeviceIoGate,
) -> CaptureStop {
    let mut wake_events = wireless.as_ref().map(EmittingFeature::listen);
    let mut shutdown = std::pin::pin!(shutdown);
    let mut watchdog = context.activity.map(Watchdog::new);
    loop {
        if !device_io.allows_io() {
            if !device_io.wait_until_allowed().await {
                return stop_for_current_publication(context.registry, context.shared);
            }
            // Time asleep is not channel idleness. Give the transport a full
            // quiet interval after visible resume and clear any pre-sleep
            // strike before considering a liveness ping.
            if let Some(watchdog) = &mut watchdog {
                watchdog.restart();
            }
        }
        tokio::select! {
            biased;

            allowed = device_io.changed() => {
                match allowed {
                    Some(true) => {
                        if let Some(watchdog) = &mut watchdog {
                            watchdog.restart();
                        }
                    }
                    Some(false) => {}
                    None => return stop_for_current_publication(context.registry, context.shared),
                }
            }
            transition = wait_for_channel_change(
                context.registry,
                context.shared,
            ) => {
                info!(index = context.device_index, capture = A::NAME, "inventory replaced or removed capture channel — restarting session");
                return transition;
            }
            _ = &mut shutdown => {
                // Shutdown and inventory replacement can become ready on the
                // same turn. Prefer the typed channel transition so teardown
                // never blindly writes through a transport already known to
                // be obsolete.
                return stop_for_current_publication(context.registry, context.shared);
            }
            event = async {
                match wake_events.as_ref() {
                    Some(events) => events.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let Some(WirelessDeviceStatusEvent::StatusBroadcast(broadcast)) = event else {
                    wake_events = None;
                    continue;
                };
                info!(?broadcast, capture = A::NAME, "device reconnected — re-arming capture");
                context.armed.reset_input_state();
                tokio::time::sleep(REARM_SETTLE_DELAY).await;
                if device_io.allows_io() {
                    context.armed.rearm().await;
                }
            }
            seq = next_activity(watchdog.as_ref()) => {
                if let Some(watchdog) = &mut watchdog {
                    watchdog.liveness.record_activity(Instant::now(), seq);
                }
            }
            () = idle_deadline(watchdog.as_ref()) => {
                let Some(watchdog) = &mut watchdog else {
                    continue;
                };
                if !watchdog
                    .liveness
                    .ping_due(Instant::now(), watchdog.activity.seq())
                {
                    continue;
                }
                let outcome = match context.root.ping(0x5a).await {
                    Err(v20::Hidpp20Error::Channel(
                        hidpp::channel::ChannelError::Timeout
                        | hidpp::channel::ChannelError::NoResponse,
                    )) => PingOutcome::AllSilent,
                    // A pong, feature error, or unsupported response all prove
                    // that this channel still receives device replies.
                    Ok(_)
                    | Err(
                        v20::Hidpp20Error::Feature(_)
                        | v20::Hidpp20Error::UnsupportedResponse,
                    ) => PingOutcome::Delivered,
                    Err(_) => PingOutcome::ChannelFailed,
                };
                if watchdog.liveness.finish_ping(
                    Instant::now(),
                    watchdog.activity.seq(),
                    outcome,
                ) == LivenessDecision::Restart {
                    warn!(index = context.device_index, capture = A::NAME, "capture channel stopped delivering — restarting session on a fresh channel");
                    return stop_for_current_publication(context.registry, context.shared);
                }
            }
        }
    }
}

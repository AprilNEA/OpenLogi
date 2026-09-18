//! Live control capture for one device: divert the device's gesture sources
//! (DPI/ModeShift, the MX dedicated gesture button and/or the MX Master 4
//! haptic panel), and the thumb wheel over HID++ and turn their events
//! into [`CapturedInput`] the GUI can dispatch.
//!
//! [`run_capture_session`] runs on the HID++ channel inventory already holds
//! open for one device, enables diversion on whichever of those controls it
//! exposes, registers one message listener, and restores every control's
//! default mapping on shutdown. Using that one channel matters: a second
//! channel to the same device would split its input-report stream, so all
//! captured controls share this session.
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! The GUI maps each [`CapturedInput`] to the user's bound action and dispatches
//! it, mirroring how the CGEventTap hook handles the side buttons. The thumb
//! wheel is special: diverting it stops native horizontal scroll, so the GUI
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when the user's thumbwheel config leaves its
//! defaults (click bound, rotation rebound, or sensitivity changed).

mod accum;
mod arm;
mod liveness;

use std::sync::{Arc, Mutex, PoisonError};

use hidpp::{
    feature::{
        CreatableFeature, EmittingFeature,
        root::RootFeature,
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use openlogi_core::binding::{ButtonId, GestureDirection};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::channel::route::DeviceRoute;
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel};

use accum::CaptureAccum;
pub(crate) use arm::enumerate_controls;
use arm::{ArmedControls, ArmedThumbwheel, arm_controls};
use liveness::{CaptureLiveness, ChannelActivity, LivenessDecision, PingOutcome};

pub use super::capture_restore::{
    CaptureChannelSlot, CaptureError, CaptureSessionFailure, CaptureSessionOutcome,
    PendingCaptureRestore,
};
use super::capture_restore::{
    CaptureStop, drop_listener_after, restore_after_stop, stop_for_current_publication,
    wait_for_channel_change,
};
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::thumbwheel::{self, WheelResolution};

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedInput {
    /// A completed swipe (or tap click) from a diverted gesture source,
    /// tagged with the source control so dispatch resolves it against that
    /// button's own direction map.
    Gesture(ButtonId, GestureDirection),
    /// A diverted button's physical down edge.
    ButtonDown(ButtonId),
    /// Thumb-wheel rotation to re-synthesise on the configured scroll axis.
    /// Emitted while the wheel is diverted (click bound, rotation rebound, or
    /// sensitivity changed).
    Scroll {
        /// Rotation in the wheel's diverted increments. Positive is always
        /// physical forward/up: arming normalises the model-specific polarity
        /// reported by `0x2150 default_dir`.
        increments: i16,
        /// What one revolution measures in each mode, so the dispatcher can
        /// scale those increments back to the wheel's native scroll amount
        /// instead of scrolling by however finely this wheel happens to
        /// report.
        resolution: WheelResolution,
    },
    /// The un-inverted polarity learned while arming a thumb wheel. This is a
    /// one-time session fact rather than user input; the agent records it for
    /// native horizontal-wheel events that the Windows hook cannot attribute
    /// to a device.
    ThumbwheelDirection {
        /// Whether a positive native delta is physical forward/up.
        positive_is_forward: bool,
    },
    /// A diverted button's physical up edge.
    ButtonUp(ButtonId),
    /// An instantaneous firmware-reported tap with no observable hold
    /// duration, such as the thumb-wheel touch sensor.
    ButtonPulse(ButtonId),
}

/// HID++-divertable standard buttons: the `0x1b04` control ID and the
/// [`ButtonId`] its press dispatches as. A button is diverted per device only
/// when its binding leaves the default, so an unbound button keeps its native
/// HID behavior (no re-synthesis needed). The Haptic Sense Panel is a gesture
/// source ([`GESTURE_SOURCE_BUTTONS`]), not a member of this table.
///
/// The two wheel-tilt CIDs are the classic "Left/Right Scroll" controls that
/// MX-line mice with a tilting main wheel (MX Anywhere 2S and friends) expose
/// as divertable — the same mechanism Options+ uses to rebind a tilt. Arming
/// only ever diverts what a device's own `getCtrlIdInfo` reports, so listing
/// them here is inert on a mouse whose wheel does not tilt.
pub const DIVERTABLE_STANDARD_BUTTONS: [(u16, ButtonId); 9] = {
    // Destructured rather than indexed: a family that gains a CID stops
    // compiling here instead of silently staying out of the table.
    let [
        back,
        back_multiplatform,
        back_multiplatform_alt,
        back_generic,
    ] = reprog_controls::BACK_CIDS;
    let [forward, forward_multiplatform] = reprog_controls::FORWARD_CIDS;
    [
        (0x0052, ButtonId::MiddleClick),
        (back, ButtonId::Back),
        (back_multiplatform, ButtonId::Back),
        (back_multiplatform_alt, ButtonId::Back),
        (back_generic, ButtonId::Back),
        (forward, ButtonId::Forward),
        (forward_multiplatform, ButtonId::Forward),
        (0x005b, ButtonId::WheelTiltLeft),
        (0x005d, ButtonId::WheelTiltRight),
    ]
};

/// HID++ gesture sources: the `0x1b04` control ID and the [`ButtonId`] it
/// delivers — the dedicated gesture button on most MX mice, and the Haptic
/// Sense Panel on MX Master 4 (two distinct physical controls). Each source in
/// gesture mode is diverted with raw-XY; one with a non-default single binding
/// instead is plain-diverted like a standard button.
pub const GESTURE_SOURCE_BUTTONS: [(u16, ButtonId); 2] = [
    (reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton),
    (reprog_controls::HAPTIC_PANEL_CID, ButtonId::HapticPanel),
];

/// Which of one device's controls a capture session should divert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureSpec {
    /// Divert the thumb wheel over `0x2150` (rotation rebind / sensitivity /
    /// click bound).
    pub capture_thumbwheel: bool,
    /// Gesture-source CIDs ([`GESTURE_SOURCE_BUTTONS`] members) to divert
    /// with raw-XY — one per source in gesture mode; empty when no HID++
    /// control gestures.
    pub divert_gesture_sources: Vec<u16>,
    /// Standard-button CIDs requested as raw-XY gesture sources. A control is
    /// armed only when its HID++ capability flags advertise raw-XY support.
    pub divert_gesture_buttons: Vec<(u16, ButtonId)>,
    /// Buttons to divert as plain presses (no raw-XY): the
    /// [`DIVERTABLE_STANDARD_BUTTONS`] and non-gesturing
    /// [`GESTURE_SOURCE_BUTTONS`] whose binding leaves the default.
    pub divert_buttons: Vec<(u16, ButtonId)>,
}

/// Capture the controls selected by `spec` on `route` until `shutdown`
/// resolves, forwarding each event to `sink`.
///
/// Each gesture source in `spec.divert_gesture_sources` is diverted with
/// raw-XY. A source not in gesture mode keeps its native behavior — unless a
/// non-default single binding puts it in `spec.divert_buttons`, in which case
/// it is diverted as a plain button (the OS hook never sees a gesture-source
/// CID, so this is the binding's only delivery path). The DPI/ModeShift
/// capture and the channel-reuse slot are independent of this.
///
/// Runs on the inventory-owned channel `registry` currently publishes for
/// `route`: sharing that connection avoids splitting HID++ replies and input
/// reports across two readers, and a registry miss
/// ([`CaptureError::DeviceNotFound`]) is retried by the caller after a later
/// inventory publication. Diverts whichever of those controls the device
/// exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped). A normal stop restores every diverted control before returning;
/// transport replacement or loss may return
/// [`CaptureSessionOutcome::RestorePending`] for the caller to retry on the
/// current inventory channel.
pub async fn run_capture_session(
    route: DeviceRoute,
    spec: CaptureSpec,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannelSlot,
    registry: &ChannelRegistry,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = registry
        .lookup(&route)
        .ok_or(CaptureError::DeviceNotFound)?;
    run_capture_session_on(
        shared,
        spec,
        sink,
        shutdown,
        channel_slot,
        registry,
        device_io,
    )
    .await
}

async fn run_capture_session_on(
    shared: SharedChannel,
    spec: CaptureSpec,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannelSlot,
    registry: &ChannelRegistry,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    device_io.ensure_allowed().map_err(CaptureError::from)?;
    let chan = Arc::clone(shared.channel());
    let device_index = shared.device_index();
    let armed = arm_controls(&chan, device_index, &spec, &shared, registry).await?;

    if let Some(direction) = armed.thumbwheel_direction() {
        let _ = sink.send(direction);
    }

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(shared.clone());
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let reprog_index = armed.reprog.as_ref().map(ReprogControlsV4::feature_index);
    let gesture_cids = armed.gesture_cids.clone();
    let gesture_button_set = armed.gesture_button_cids.clone();
    let thumb_index = armed
        .thumb
        .as_ref()
        .map(|thumb| thumb.wheel.feature_index());
    let thumb_resolution = armed
        .thumb
        .as_ref()
        .map_or(WheelResolution::UNKNOWN, ArmedThumbwheel::resolution);
    let dpi_set = armed.dpi_cids.clone();
    let button_set = armed.button_cids.clone();
    let activity = Arc::new(ChannelActivity::default());
    let listener = chan.add_msg_listener_guarded({
        let accum = Arc::clone(&accum);
        let activity = Arc::clone(&activity);
        let sink = sink.clone();
        move |raw, matched| {
            // Every parsed inbound HID++ report proves this channel's read
            // path is alive, including responses matched to another request.
            activity.record();
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            if let Some(idx) = reprog_index
                && let Some(event) = reprog_controls::decode_event(&msg, device_index, idx)
            {
                // Recover the guard even if a prior holder panicked — the
                // critical section is panic-free, so the data is consistent.
                let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
                acc.on_event(
                    event,
                    &gesture_cids,
                    &dpi_set,
                    &gesture_button_set,
                    &button_set,
                    &sink,
                );
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(&msg, device_index, idx)
                && let Some(input) = thumbwheel_input(event, thumb_resolution)
            {
                let _ = sink.send(input);
            }
        }
    });

    // Liveness watchdog: this session's channel is the sole delivery path for
    // every diverted control, and a channel whose input-report delivery dies
    // (observed on macOS with concurrent opens of one node: writes accepted,
    // replies and events silently routed elsewhere) turns every captured
    // button to dead air with nothing to notice. Ping the device through this
    // channel; consecutive all-silent pings mean the channel — not the device
    // — is gone (a sleeping/unreachable device can still send an HID++ error
    // reply, which proves delivery and resets the count). A transport/setup
    // error proves neither delivery nor silence, so it restarts immediately.
    // Exiting lets the manager re-arm on a fresh channel.
    let root = RootFeature::new(Arc::clone(&chan), device_index, 0);
    let wireless = root
        .get_feature(WirelessDeviceStatusFeature::ID)
        .await
        .ok()
        .flatten()
        .map(|info| WirelessDeviceStatusFeature::new(Arc::clone(&chan), device_index, info.index));
    log_capture_active(device_index, &armed, wireless.is_some());
    let stop = monitor_capture(
        CaptureMonitor {
            root: &root,
            armed: &armed,
            accum: &accum,
            device_index,
            registry,
            shared: &shared,
            activity: &activity,
        },
        wireless,
        shutdown,
        device_io,
    )
    .await;

    // The slot is one last-writer-wins cell shared by every session, so a
    // sibling may have published its own channel after ours. Clear it only
    // while it still holds *this* session's channel — evicting the sibling's
    // would silently demote its DPI/SmartShift writes to the fresh-open slow
    // path.
    if let Ok(mut slot) = channel_slot.write()
        && slot
            .as_ref()
            .is_some_and(|shared| Arc::ptr_eq(shared.channel(), &chan))
    {
        *slot = None;
    }
    let outcome = finish_capture(listener, stop, armed, shared, registry).await;
    debug!(index = device_index, "control capture stopped");
    Ok(outcome)
}

/// Restore or hand off one stopped session while its listener still owns every
/// diverted input report.
async fn finish_capture<T>(
    listener: T,
    stop: CaptureStop,
    armed: ArmedControls,
    retired: SharedChannel,
    registry: &ChannelRegistry,
) -> CaptureSessionOutcome {
    let pending = armed.into_pending(&retired);
    drop_listener_after(listener, restore_after_stop(stop, pending, registry)).await
}

/// The single input one diverted thumb-wheel report stands for, if any.
///
/// A report is a roll *or* a tap, never both, and `0x2150` says which: the
/// wheel's touch sensor sets `single_tap` for the finger that turned the
/// wheel, so every report from `Start` through `Stop` carries a tap bit that
/// belongs to the roll rather than to the user. `Stop` is the one that needs
/// the status field — it is the release, so it reports no rotation of its own
/// and is otherwise indistinguishable from a tap on a settled wheel.
///
/// A report's own rotation is checked alongside the status rather than
/// through it: both are direct statements that this report is part of a roll,
/// and taking either keeps the roll recognised on a wheel whose firmware
/// leaves byte 4 at zero.
fn thumbwheel_input(
    event: thumbwheel::ThumbwheelEvent,
    resolution: WheelResolution,
) -> Option<CapturedInput> {
    if event.rotation != 0 {
        return Some(CapturedInput::Scroll {
            increments: event.rotation,
            resolution,
        });
    }
    if event.rotation_status.is_rolling() {
        return None;
    }
    event
        .single_tap
        .then_some(CapturedInput::ButtonPulse(ButtonId::Thumbwheel))
}

fn log_capture_active(device_index: u8, armed: &ArmedControls, wake_rearm: bool) {
    info!(
        index = device_index,
        gesture_sources = armed.gesture_cids.len(),
        gesture_buttons = armed.gesture_button_cids.len(),
        dpi_buttons = armed.dpi_cids.len(),
        buttons = armed.button_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        wake_rearm,
        "control capture active"
    );
}

/// Borrowed state used while monitoring one armed capture session.
struct CaptureMonitor<'a> {
    root: &'a RootFeature,
    armed: &'a ArmedControls,
    accum: &'a Arc<Mutex<CaptureAccum>>,
    device_index: u8,
    registry: &'a ChannelRegistry,
    shared: &'a SharedChannel,
    activity: &'a ChannelActivity,
}

/// Keep a capture session alive and reapply its volatile diversions whenever
/// the device announces a reconnect. Returns only the typed reason capture
/// stopped; restoration performs a fresh registry lookup after monitoring.
async fn monitor_capture(
    context: CaptureMonitor<'_>,
    wireless: Option<WirelessDeviceStatusFeature>,
    shutdown: oneshot::Receiver<()>,
    mut device_io: DeviceIoGate,
) -> CaptureStop {
    let mut wake_events = wireless.as_ref().map(EmittingFeature::listen);
    let mut shutdown = std::pin::pin!(shutdown);
    let mut liveness = CaptureLiveness::new(tokio::time::Instant::now(), context.activity.seq());
    loop {
        if !device_io.allows_io() {
            if !device_io.wait_until_allowed().await {
                return stop_for_current_publication(context.registry, context.shared);
            }
            // Time asleep is not channel idleness. Give the transport a full
            // quiet interval after visible resume and clear any pre-sleep
            // strike before considering a liveness ping.
            liveness.record_activity(tokio::time::Instant::now(), context.activity.seq());
        }
        let activity_seq = liveness.activity_seq();
        let idle_deadline = liveness.idle_deadline();
        tokio::select! {
            biased;

            allowed = device_io.changed() => {
                match allowed {
                    Some(true) => liveness.record_activity(
                        tokio::time::Instant::now(),
                        context.activity.seq(),
                    ),
                    Some(false) => {}
                    None => return stop_for_current_publication(context.registry, context.shared),
                }
            }
            transition = wait_for_channel_change(
                context.registry,
                context.shared,
            ) => {
                info!(index = context.device_index, "inventory replaced or removed capture channel — restarting session");
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
                info!(?broadcast, "device reconnected — re-arming control capture");
                *context.accum.lock().unwrap_or_else(PoisonError::into_inner) =
                    CaptureAccum::default();
                context.armed.rearm(&device_io).await;
            }
            seq = context.activity.changed_after(activity_seq) => {
                liveness.record_activity(tokio::time::Instant::now(), seq);
            }
            () = tokio::time::sleep_until(idle_deadline) => {
                if !liveness.ping_due(
                    tokio::time::Instant::now(),
                    context.activity.seq(),
                ) {
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
                if liveness.finish_ping(
                    tokio::time::Instant::now(),
                    context.activity.seq(),
                    outcome,
                ) == LivenessDecision::Restart {
                    warn!(index = context.device_index, "capture channel stopped delivering — restarting session on a fresh channel");
                    return stop_for_current_publication(context.registry, context.shared);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

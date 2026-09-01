//! Live control capture for one device: divert the device's gesture sources
//! (the MX dedicated gesture button and/or the MX Master 4 haptic panel), the
//! DPI/ModeShift button, the thumb wheel, and raw touchpads over HID++ and turn
//! their events into [`CapturedInput`] the Agent can dispatch.
//!
//! [`run_capture_session`] holds a single HID++ channel open for one device,
//! enables diversion on whichever of those controls it exposes, registers its
//! event listeners, and restores every control's default mapping on shutdown.
//! Using one channel matters: a second channel to the same device would split
//! its input-report stream, so all captured controls share this session.
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! Agent-core maps each [`CapturedInput`] to the user's bound action and
//! dispatches it, mirroring how the CGEventTap hook handles the side buttons.
//! The thumb wheel is special: diverting it stops native horizontal scroll, so the Agent
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when the user's thumbwheel config leaves its
//! defaults (click bound, rotation rebound, or sensitivity changed).

mod liveness;
mod touchpad;

use std::sync::{Arc, Mutex, PoisonError};

use hidpp::{
    channel::{HidppChannel, MessageListenerGuard},
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        root::RootFeature,
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use openlogi_core::touchpad::TouchFrame;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::backend::{BackendError, HidBackend};
use crate::channel::route::{DeviceRoute, open_route_channel};
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel};

use liveness::{CaptureLiveness, ChannelActivity, LivenessDecision, PingOutcome};

#[cfg(test)]
use super::capture_restore::undivert_change;
use super::capture_restore::{
    ArmedReporting, CaptureStop, ReprogRestore, divert_change, drop_listener_after,
    restore_after_stop, rollback_capture_start, stop_for_current_publication,
    wait_for_channel_change,
};
pub use super::capture_restore::{
    CaptureChannel, CaptureSessionFailure, CaptureSessionOutcome, GestureError,
    PendingCaptureRestore,
};
use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::thumbwheel::{self, Thumbwheel, ThumbwheelInfo, WheelDirection, WheelResolution};

pub use touchpad::{
    ArmedRawMode, OPENLOGI_RAW_REPORT_FLAGS, RawModeJournal, TouchpadCaptureError,
    TouchpadFrameStream, TouchpadJournalError, TouchpadJournalStore, TouchpadStreamError,
    TouchpadStreamEvent,
};

/// How often an armed touchpad confirms that no competing manager has changed
/// its volatile raw-report mode on the same HID++ channel.
const TOUCHPAD_RAW_MODE_POLL: std::time::Duration = std::time::Duration::from_secs(1);
/// One input captured from the active device.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// One complete normalized `0x6100` touchpad frame.
    TouchpadFrame(TouchFrame),
    /// The active touchpad contact set ended.
    TouchpadEnd,
    /// Unsupported contact count or malformed data cancelled the touchpad stroke.
    TouchpadCancel,
    /// Invalid or incomplete `0x6100` logical frames dropped by strict
    /// assembly. Diagnostic only; action dispatch ignores it.
    TouchpadDroppedFrames(u64),
}

/// The hold that owns raw-XY motion, or the absence of one. Raw-XY reports
/// carry no source attribution, so the first held source owns the accumulated
/// motion until it is released (first hold wins); the per-hold qualifiers live
/// inside the variant so none can outlive the hold it belongs to.
#[derive(Default)]
enum HoldState {
    /// No armed gesture source is held: raw-XY reports are stray and dropped.
    #[default]
    Idle,
    /// `cid` began the current hold; its events dispatch as `button`. When the
    /// holder releases, a still-held source takes the hold over.
    Holding {
        /// The `0x1b04` control that owns this hold.
        cid: u16,
        /// The [`ButtonId`] the hold's gestures dispatch as.
        button: ButtonId,
        /// Mid-swipe travel accumulated over this hold.
        swipe: SwipeAccumulator,
        /// A second armed source is held alongside the holder. Overlap motion
        /// could belong to either control — dropped until the overlap ends.
        overlap: bool,
        /// The hold's next raw-XY sample must be dropped: the haptic panel's
        /// first sample after contact is an absolute position jump, not a
        /// delta (see [`reprog_controls::HAPTIC_PANEL_CID`]).
        skip_first_raw_xy: bool,
    },
}

/// Begin a hold for `cid`, its swipe accumulator started fresh.
fn begin_hold(cid: u16, button: ButtonId, overlap: bool, skip_first_raw_xy: bool) -> HoldState {
    let mut swipe = SwipeAccumulator::default();
    swipe.begin();
    HoldState::Holding {
        cid,
        button,
        swipe,
        overlap,
        skip_first_raw_xy,
    }
}

/// Movement + button state accumulated across messages. Lives behind a `Mutex`
/// because the channel's read thread invokes the listener by shared reference.
#[derive(Default)]
struct CaptureAccum {
    /// The hold owning raw-XY motion, if any (see [`HoldState`]).
    hold: HoldState,
    /// The armed gesture sources held in the last event, for edge detection:
    /// a source not previously held that becomes the holder is a fresh touch
    /// (the haptic panel's first sample is then a contact jump to discard).
    gestures_down: Vec<u16>,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
    /// Diverted standard-button CIDs held in the last event.
    buttons_down: Vec<u16>,
}

#[cfg(test)]
impl CaptureAccum {
    /// Test-only seam mirroring [`SwipeAccumulator::backdate_hold_for_test`]
    /// for the current hold. A no-op while idle.
    fn backdate_hold_for_test(&mut self) {
        if let HoldState::Holding { swipe, .. } = &mut self.hold {
            swipe.backdate_hold_for_test();
        }
    }
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
pub const DIVERTABLE_STANDARD_BUTTONS: [(u16, ButtonId); 9] = [
    (0x0052, ButtonId::MiddleClick),
    (0x0053, ButtonId::Back),
    (0x00BD, ButtonId::Back),
    (0x00CE, ButtonId::Back),
    (0x00DB, ButtonId::Back),
    (0x0056, ButtonId::Forward),
    (0x00CF, ButtonId::Forward),
    (0x005b, ButtonId::WheelTiltLeft),
    (0x005d, ButtonId::WheelTiltRight),
];

/// HID++ gesture sources: the `0x1b04` control ID and the [`ButtonId`] it
/// delivers — the dedicated gesture button on most MX mice, and the Haptic
/// Sense Panel on MX Master 4 (two distinct physical controls). Each source in
/// gesture mode is diverted with raw-XY; one with a non-default single binding
/// instead is plain-diverted like a standard button.
pub const GESTURE_SOURCE_BUTTONS: [(u16, ButtonId); 2] = [
    (reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton),
    (reprog_controls::HAPTIC_PANEL_CID, ButtonId::HapticPanel),
];

/// Lifetime and control scope of one capture session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureSessionMode {
    /// Arm all controls requested by [`CaptureSpec`] and listen until stopped.
    #[default]
    Continuous,
    /// Arm only the raw touchpad and listen until stopped.
    TouchpadOnly,
    /// Recover a durable raw-touchpad journal record, then exit without
    /// diverting or listening to any controls.
    TouchpadRecovery,
}

/// Which of one device's controls a capture session should divert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureSpec {
    /// Whether this is a normal session, explicit touchpad-only capture, or a
    /// one-shot durable-journal recovery.
    pub mode: CaptureSessionMode,
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
    /// Enable `0x6100` raw touchpad reports. This is independent from every
    /// button diversion and defaults off.
    pub capture_touchpad: bool,
    /// Stable serial/non-zero-unit identity used for the durable raw-mode
    /// journal. Raw mode is never armed without one.
    pub touchpad_journal_id: Option<String>,
}

/// Runtime resources consumed by one control-capture session.
pub struct CaptureSessionRequest {
    /// Controls and capture mode to arm.
    pub spec: CaptureSpec,
    /// Durable raw-touchpad ownership journal, when touchpad capture is used.
    pub touchpad_journal: Option<Arc<dyn TouchpadJournalStore>>,
    /// Destination for captured input events.
    pub sink: mpsc::UnboundedSender<CapturedInput>,
    /// Signal that asks the session to restore its controls and stop.
    pub shutdown: oneshot::Receiver<()>,
    /// Shared publication slot for the session's active HID++ channel.
    pub channel_slot: CaptureChannel,
    /// Host sleep/wake gate that prevents device I/O while suspended.
    pub device_io: DeviceIoGate,
}

/// Run the capture mode selected by `request.spec` on `route`, forwarding each
/// event to `request.sink`.
///
/// Each gesture source in `request.spec.divert_gesture_sources` is diverted with
/// raw-XY. A source not in gesture mode keeps its native behavior — unless a
/// non-default single binding puts it in `request.spec.divert_buttons`, in
/// which case it is diverted as a plain button (the OS hook never sees a
/// gesture-source CID, so this is the binding's only delivery path). The
/// DPI/ModeShift capture and the channel-reuse slot are independent of this.
///
/// A continuous or touchpad-only session holds one HID++ channel and listens
/// until `request.shutdown` fires (or its sender is dropped), then restores
/// every control it diverted. Touchpad recovery opens the device only long
/// enough to compare-and-restore a durable journal record and returns
/// immediately. A normal stop restores every diverted control before
/// returning; transport replacement or loss may return
/// [`CaptureSessionOutcome::RestorePending`] for the caller to retry on the
/// current inventory channel.
pub async fn run_capture_session(
    backend: &dyn HidBackend,
    route: DeviceRoute,
    request: CaptureSessionRequest,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    if !request.device_io.allows_io() {
        return Err(GestureError::Hid(BackendError::Backend(
            "host device I/O is suspended".into(),
        ))
        .into());
    }
    let chan = open_route_channel(backend, &route)
        .await
        .map_err(GestureError::from)?
        .ok_or(GestureError::DeviceNotFound)?;
    let shared = SharedChannel::new(chan, route.clone());
    run_capture_session_on(shared, request, None).await
}

/// Capture through the inventory-owned channel currently published for
/// `route`. Sharing that connection avoids splitting HID++ replies and input
/// reports across two readers; a registry miss is retried by the caller after
/// a later inventory publication.
pub async fn run_capture_session_with_registry_spec(
    route: DeviceRoute,
    request: CaptureSessionRequest,
    registry: &ChannelRegistry,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = registry
        .lookup(&route)
        .ok_or(GestureError::DeviceNotFound)?;
    run_capture_session_on(shared, request, Some(registry)).await
}

async fn run_capture_session_on(
    shared: SharedChannel,
    request: CaptureSessionRequest,
    registry: Option<&ChannelRegistry>,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let CaptureSessionRequest {
        spec,
        touchpad_journal,
        sink,
        shutdown,
        channel_slot,
        device_io,
    } = request;
    if !device_io.allows_io() {
        return Err(GestureError::Hid(BackendError::Backend(
            "host device I/O is suspended".into(),
        ))
        .into());
    }
    let chan = Arc::clone(shared.channel());
    let device_index = shared.device_index();
    let mut armed = arm_controls(
        &chan,
        device_index,
        &spec,
        touchpad_journal.as_ref(),
        &shared,
        registry,
    )
    .await?;

    if spec.mode == CaptureSessionMode::TouchpadRecovery {
        debug!(index = device_index, "touchpad raw-mode recovery complete");
        return Ok(CaptureSessionOutcome::Restored);
    }

    if let Some(direction) = armed.thumbwheel_direction() {
        let _ = sink.send(direction);
    }

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(shared.clone());
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let activity = Arc::new(ChannelActivity::default());
    let listener = add_control_listener(&chan, &armed, &accum, device_index, &sink, &activity);

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
    let (stop, session_error) = monitor_capture(
        CaptureMonitor {
            root: &root,
            armed: &mut armed,
            accum: &accum,
            device_index,
            registry,
            shared: &shared,
            activity: &activity,
            sink: &sink,
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
    match session_error {
        None => Ok(outcome),
        Some(error) => Err(match outcome {
            CaptureSessionOutcome::Restored => CaptureSessionFailure::clean(error),
            CaptureSessionOutcome::RestorePending(pending) => {
                CaptureSessionFailure::with_pending(error, pending)
            }
        }),
    }
}

/// Restore or hand off one stopped session while its listener still owns every
/// diverted input report.
async fn finish_capture<T>(
    listener: T,
    stop: CaptureStop,
    armed: ArmedControls,
    retired: SharedChannel,
    registry: Option<&ChannelRegistry>,
) -> CaptureSessionOutcome {
    let mut armed = armed;
    armed.disarm_touchpad().await;
    let pending = armed.into_pending(&retired);
    drop_listener_after(
        listener,
        restore_after_stop(stop, pending, &retired, registry),
    )
    .await
}

fn add_control_listener(
    channel: &Arc<HidppChannel>,
    armed: &ArmedControls,
    accum: &Arc<Mutex<CaptureAccum>>,
    device_index: u8,
    sink: &mpsc::UnboundedSender<CapturedInput>,
    activity: &Arc<ChannelActivity>,
) -> MessageListenerGuard {
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
    let accum = Arc::clone(accum);
    let activity = Arc::clone(activity);
    let sink = sink.clone();
    channel.add_msg_listener_guarded(move |raw, matched| {
        // Every parsed inbound HID++ report proves this channel's read path is
        // alive, including responses matched to another request.
        activity.record();
        if matched {
            return;
        }
        let message = v20::Message::from(raw);
        if let Some(index) = reprog_index
            && let Some(event) = reprog_controls::decode_event(&message, device_index, index)
        {
            // Recover the guard even if a prior holder panicked — the
            // critical section is panic-free, so the data is consistent.
            let mut accum = accum.lock().unwrap_or_else(PoisonError::into_inner);
            handle_reprog_with_gesture_buttons(
                &mut accum,
                event,
                &gesture_cids,
                &dpi_set,
                &gesture_button_set,
                &button_set,
                &sink,
            );
            return;
        }
        if let Some(index) = thumb_index
            && let Some(event) = thumbwheel::decode_event(&message, device_index, index)
            && let Some(input) = thumbwheel_input(event, thumb_resolution)
        {
            let _ = sink.send(input);
        }
    })
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

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
#[derive(Default)]
struct ArmedControls {
    /// `0x1b04` accessor, present when the device exposes it.
    reprog: Option<ReprogControlsV4>,
    /// The gesture-source CIDs diverted with raw-XY reporting: the
    /// `spec.divert_gesture_sources` members the device exposes.
    gesture_cids: Vec<u16>,
    /// Raw-XY-capable standard-button CIDs diverted as gesture sources.
    gesture_button_cids: Vec<(u16, ButtonId)>,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// Standard-button CIDs diverted per the session's [`CaptureSpec`], with
    /// the [`ButtonId`] each dispatches as.
    button_cids: Vec<(u16, ButtonId)>,
    /// Original reporting state for every diverted `0x1b04` control.
    reporting: Vec<ArmedReporting>,
    /// `0x2150` accessor and the information read while diverting it, present
    /// when the thumb wheel is diverted.
    thumb: Option<ArmedThumbwheel>,
    /// `0x6100` accessor, normalizer, and raw-mode ownership when touchpad
    /// capture is armed.
    touchpad: Option<ArmedTouchpad>,
}

struct ArmedThumbwheel {
    wheel: Thumbwheel,
    info: Option<ThumbwheelInfo>,
}

impl ArmedThumbwheel {
    fn resolution(&self) -> WheelResolution {
        self.info
            .map_or(WheelResolution::UNKNOWN, |info| info.resolution)
    }

    fn direction(&self) -> WheelDirection {
        if self.info.is_some_and(|info| !info.positive_is_forward()) {
            WheelDirection::Inverted
        } else {
            WheelDirection::Default
        }
    }
}

struct ArmedTouchpad {
    feature: hidpp::feature::touchpad_raw_xy::TouchpadRawXyFeature,
    stream: TouchpadFrameStream,
    raw_mode: ArmedRawMode,
    gestures: Option<hidpp::feature::gestures2::Gestures2Feature>,
    journal: Arc<dyn TouchpadJournalStore>,
    journal_id: String,
}

impl ArmedTouchpad {
    fn push(
        &mut self,
        event: hidpp::feature::touchpad_raw_xy::TouchpadRawEvent,
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        let hidpp::feature::touchpad_raw_xy::TouchpadRawEvent::DualXy(report) = event else {
            if let hidpp::feature::touchpad_raw_xy::TouchpadRawEvent::GestureHandledByDevice {
                payload,
            } = event
            {
                tracing::debug!(?payload, "firmware handled a gesture itself");
            }
            return;
        };
        for event in self.stream.push(report, std::time::Instant::now()) {
            send_touchpad_event(event, sink);
        }
    }

    async fn verify_raw_mode(&self) -> Result<(), GestureError> {
        let actual = self
            .feature
            .get_raw_report_state()
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?
            .bits();
        let expected = self.raw_mode.expected();
        if actual != expected {
            return Err(GestureError::TouchpadRawModeConflict { expected, actual });
        }
        Ok(())
    }
}

impl ArmedControls {
    /// Build the one-time polarity fact learned while arming the thumb wheel.
    fn thumbwheel_direction(&self) -> Option<CapturedInput> {
        let positive_is_forward = self
            .thumb
            .as_ref()?
            .info
            .map(ThumbwheelInfo::positive_is_forward)?;
        Some(CapturedInput::ThumbwheelDirection {
            positive_is_forward,
        })
    }

    /// Convert all armed firmware state into the one capability that can
    /// release it. Consuming `self` prevents a session and a restore retry from
    /// both claiming ownership at once.
    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        let Self {
            reprog,
            reporting,
            thumb,
            ..
        } = self;
        let reprog =
            reprog.and_then(|controls| ReprogRestore::new(controls.feature_index(), reporting));
        PendingCaptureRestore::new(
            retired,
            reprog,
            thumb.as_ref().map(|thumb| thumb.wheel.feature_index()),
        )
    }

    async fn disarm_touchpad(&mut self) {
        let Some(touchpad) = self.touchpad.take() else {
            return;
        };
        if let Err(error) = touchpad
            .raw_mode
            .disarm(
                &touchpad.feature,
                touchpad.gestures.as_ref(),
                touchpad.journal.as_ref(),
                &touchpad.journal_id,
            )
            .await
        {
            warn!(%error, "touchpad raw-mode restore failed — journal retained for recovery");
        }
    }

    /// Reapply volatile diversion after a wireless reconnect broadcast. The
    /// broadcast can precede the device accepting feature writes, so allow a
    /// short settling window like the keyboard capture path does.
    async fn rearm(&self, device_io: &DeviceIoGate) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !device_io.allows_io() {
            return;
        }
        if let Some(rc) = self.reprog.as_ref() {
            for &reporting in &self.reporting {
                let raw_xy = self.gesture_cids.contains(&reporting.cid)
                    || self
                        .gesture_button_cids
                        .iter()
                        .any(|&(cid, _)| cid == reporting.cid);
                let change = divert_change(reporting.original, raw_xy);
                if let Err(error) = rc.set_cid_reporting_full(reporting.cid, change).await {
                    warn!(
                        cid = format_args!("{:#06x}", reporting.cid),
                        ?error,
                        "re-divert after wake failed"
                    );
                }
            }
        }
        if let Some(thumb) = self.thumb.as_ref()
            && let Err(error) = thumb.wheel.divert(thumb.direction()).await
        {
            warn!(?error, "thumb-wheel re-divert after wake failed");
        }
    }
}

fn log_capture_active(device_index: u8, armed: &ArmedControls, wake_rearm: bool) {
    info!(
        index = device_index,
        gesture_sources = armed.gesture_cids.len(),
        gesture_buttons = armed.gesture_button_cids.len(),
        dpi_buttons = armed.dpi_cids.len(),
        buttons = armed.button_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        touchpad = armed.touchpad.is_some(),
        wake_rearm,
        "control capture active"
    );
}

/// Borrowed state used while monitoring one armed capture session.
struct CaptureMonitor<'a> {
    root: &'a RootFeature,
    armed: &'a mut ArmedControls,
    accum: &'a Arc<Mutex<CaptureAccum>>,
    device_index: u8,
    registry: Option<&'a ChannelRegistry>,
    shared: &'a SharedChannel,
    activity: &'a ChannelActivity,
    sink: &'a mpsc::UnboundedSender<CapturedInput>,
}

impl CaptureMonitor<'_> {
    fn stop(&self) -> CaptureStop {
        stop_for_current_publication(self.registry, self.shared)
    }

    fn stop_with_error(&self, error: GestureError) -> (CaptureStop, Option<GestureError>) {
        (self.stop(), Some(error))
    }

    fn ping_due(&self, liveness: &mut CaptureLiveness) -> bool {
        liveness.ping_due(tokio::time::Instant::now(), self.activity.generation())
    }

    fn push_touchpad_event(&mut self, event: hidpp::feature::touchpad_raw_xy::TouchpadRawEvent) {
        if let Some(touchpad) = self.armed.touchpad.as_mut() {
            touchpad.push(event, self.sink);
        }
    }

    fn flush_touchpad_end(&mut self) {
        if let Some(touchpad) = self.armed.touchpad.as_mut() {
            for event in touchpad.stream.poll_end(std::time::Instant::now()) {
                send_touchpad_event(event, self.sink);
            }
        }
    }

    async fn raw_mode_error(&self) -> Option<GestureError> {
        let touchpad = self.armed.touchpad.as_ref()?;
        match touchpad.verify_raw_mode().await {
            Ok(()) => None,
            Err(error) => {
                log_raw_mode_failure(self.device_index, &error);
                Some(error)
            }
        }
    }
}

fn log_raw_mode_failure(device_index: u8, error: &GestureError) {
    if matches!(error, GestureError::TouchpadRawModeConflict { .. }) {
        warn!(
            index = device_index,
            error = %error,
            "touchpad raw mode changed externally — stopping capture without rearming"
        );
    } else {
        warn!(
            index = device_index,
            error = %error,
            "touchpad raw-mode verification failed — restarting capture"
        );
    }
}

async fn wait_for_capture_io(
    context: &CaptureMonitor<'_>,
    device_io: &mut DeviceIoGate,
    liveness: &mut CaptureLiveness,
) -> Option<CaptureStop> {
    if device_io.allows_io() {
        return None;
    }
    if !device_io.wait_until_allowed().await {
        return Some(context.stop());
    }
    // Time asleep is not channel idleness. Give the transport a full quiet
    // interval after visible resume and clear any pre-sleep strike.
    liveness.record_activity(tokio::time::Instant::now(), context.activity.generation());
    None
}

async fn finish_liveness_ping(
    context: &CaptureMonitor<'_>,
    liveness: &mut CaptureLiveness,
) -> LivenessDecision {
    let outcome = match context.root.ping(0x5a).await {
        Err(v20::Hidpp20Error::Channel(
            hidpp::channel::ChannelError::Timeout | hidpp::channel::ChannelError::NoResponse,
        )) => PingOutcome::AllSilent,
        // A pong, feature error, or unsupported response all prove that this
        // channel still receives device replies.
        Ok(_) | Err(v20::Hidpp20Error::Feature(_) | v20::Hidpp20Error::UnsupportedResponse) => {
            PingOutcome::Delivered
        }
        Err(_) => PingOutcome::ChannelFailed,
    };
    liveness.finish_ping(
        tokio::time::Instant::now(),
        context.activity.generation(),
        outcome,
    )
}

/// Keep a capture session alive and reapply its volatile diversions whenever
/// the device announces a reconnect. Returns only the typed reason capture
/// stopped; restoration performs a fresh registry lookup after monitoring.
async fn monitor_capture(
    mut context: CaptureMonitor<'_>,
    wireless: Option<WirelessDeviceStatusFeature>,
    shutdown: oneshot::Receiver<()>,
    mut device_io: DeviceIoGate,
) -> (CaptureStop, Option<GestureError>) {
    let mut wake_events = wireless.as_ref().map(EmittingFeature::listen);
    let touchpad_events = context
        .armed
        .touchpad
        .as_ref()
        .map(|touchpad| touchpad.feature.listen());
    let mut shutdown = std::pin::pin!(shutdown);
    let mut liveness =
        CaptureLiveness::new(tokio::time::Instant::now(), context.activity.generation());
    let mut raw_mode_ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + TOUCHPAD_RAW_MODE_POLL,
        TOUCHPAD_RAW_MODE_POLL,
    );
    loop {
        // Keep passive listeners and raw-mode ownership intact through sleep.
        // In particular, do not restore or verify controls during DarkWake.
        // A deliberate shutdown may time out while suspended; the durable
        // touchpad journal lets the next launch complete restoration.
        if let Some(stop) = wait_for_capture_io(&context, &mut device_io, &mut liveness).await {
            return (stop, None);
        }
        let activity_generation = liveness.activity_generation();
        let idle_deadline = liveness.idle_deadline();
        tokio::select! {
            biased;

            allowed = device_io.changed() => {
                match allowed {
                    Some(true) => liveness.record_activity(
                        tokio::time::Instant::now(),
                        context.activity.generation(),
                    ),
                    Some(false) => {}
                    None => return (context.stop(), None),
                }
            }
            transition = wait_for_channel_change(context.registry, context.shared) => {
                info!(index = context.device_index, "inventory replaced or removed capture channel — restarting session");
                return (transition, None);
            }
            event = async {
                match touchpad_events.as_ref() {
                    Some(events) => events.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Ok(event) => context.push_touchpad_event(event),
                    Err(error) => {
                        return context.stop_with_error(GestureError::Hidpp(format!(
                            "touchpad event stream closed: {error}"
                        )));
                    }
                }
            }
            () = async {
                if context.armed.touchpad.is_some() {
                    tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                context.flush_touchpad_end();
            }
            _ = raw_mode_ticker.tick(), if context.armed.touchpad.is_some() => {
                if let Some(error) = context.raw_mode_error().await {
                    return context.stop_with_error(error);
                }
            }
            _ = &mut shutdown => {
                // Shutdown and inventory replacement can become ready on the
                // same turn. Prefer the typed channel transition so teardown
                // never blindly writes through a transport already known to
                // be obsolete.
                return (context.stop(), None);
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
            generation = context.activity.changed_after(activity_generation) => {
                liveness.record_activity(tokio::time::Instant::now(), generation);
            }
            () = tokio::time::sleep_until(idle_deadline) => {
                if !context.ping_due(&mut liveness) {
                    continue;
                }
                if finish_liveness_ping(&context, &mut liveness).await == LivenessDecision::Restart {
                    warn!(index = context.device_index, "capture channel stopped delivering — restarting session on a fresh channel");
                    return (context.stop(), None);
                }
            }
        }
    }
}

/// Resolve features off the device's root and divert the controls `spec`
/// selects: the gesture sources (raw-XY), DPI/ModeShift buttons and rebindable
/// standard buttons over `0x1b04`, and the thumb wheel over `0x2150`. The
/// root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
///
/// A failure mid-way tries to hand every possibly-diverted control back to the
/// firmware. If compensation is incomplete, the returned failure carries an
/// opaque restore capability for the manager to retain and retry.
async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    touchpad_journal: Option<&Arc<dyn TouchpadJournalStore>>,
    shared: &SharedChannel,
    registry: Option<&ChannelRegistry>,
) -> Result<ArmedControls, CaptureSessionFailure> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;
    let mut armed = ArmedControls::default();
    if let Err(error) =
        arm_controls_into(&device, chan, slot, spec, touchpad_journal, &mut armed).await
    {
        armed.disarm_touchpad().await;
        let pending = armed.into_pending(shared);
        return Err(rollback_capture_start(error, pending, shared, registry).await);
    }
    if armed.gesture_cids.is_empty()
        && armed.gesture_button_cids.is_empty()
        && armed.dpi_cids.is_empty()
        && armed.button_cids.is_empty()
        && armed.thumb.is_none()
        && armed.touchpad.is_none()
    {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(armed)
}

/// The fallible arming steps of [`arm_controls`], recording ownership before
/// each write. A transport failure cannot prove whether firmware applied that
/// write, so rollback deliberately includes the uncertain current control.
async fn arm_controls_into(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    touchpad_journal: Option<&Arc<dyn TouchpadJournalStore>>,
    armed: &mut ArmedControls,
) -> Result<(), GestureError> {
    if spec.mode != CaptureSessionMode::Continuous {
        return arm_touchpad(device, chan, slot, spec, touchpad_journal, armed).await;
    }

    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let rc = ReprogControlsV4::new(Arc::clone(chan), slot, info.index);
        let controls = enumerate_controls(&rc).await?;
        // Register an accessor before the first divert, so a failure on any
        // divert (including the first) can become a restore capability.
        armed.reprog = Some(rc.clone());

        // Divert each gesture-mode source; a source not listed stays native
        // (an idle HID++ control must not be captured-and-dropped).
        for &cid in &spec.divert_gesture_sources {
            if controls.iter().any(|c| c.cid == cid && c.supports_raw_xy()) {
                arm_reprog_control(&rc, cid, true, &mut armed.reporting).await?;
                armed.gesture_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_gesture_buttons {
            if let Some(control) = controls.iter().find(|c| c.cid == cid)
                && control.is_divertable()
                && control.supports_raw_xy()
            {
                arm_reprog_control(&rc, cid, true, &mut armed.reporting).await?;
                armed.gesture_button_cids.push((cid, button));
            }
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                arm_reprog_control(&rc, cid, false, &mut armed.reporting).await?;
                armed.dpi_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_buttons {
            // The plan never lists a raw-XY-diverted gesture source, but
            // guard anyway: a plain (divert, no raw-XY) write here would strip
            // the raw-XY reporting armed above.
            if armed.gesture_cids.contains(&cid)
                || armed
                    .gesture_button_cids
                    .iter()
                    .any(|&(gesture_cid, _)| gesture_cid == cid)
            {
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                arm_reprog_control(&rc, cid, false, &mut armed.reporting).await?;
                armed.button_cids.push((cid, button));
            }
        }
    }

    if spec.capture_thumbwheel
        && let Some(info) = device
            .root()
            .get_feature(thumbwheel::FEATURE_ID)
            .await
            .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let tw = Thumbwheel::new(Arc::clone(chan), slot, info.index);
        // Consume the getInfo error here, before the next await: Hidpp20Error
        // isn't Send, so holding it across an await would make this future
        // (spawned on tokio) non-Send.
        let wheel_info = match tw.get_info().await {
            Ok(twinfo) => Some(twinfo),
            Err(e) => {
                warn!(error = ?e, "thumb wheel getInfo failed");
                None
            }
        };
        // Divert whenever capture was requested: rotation rebinds and the
        // sensitivity multiplier need the diverted event stream even on wheels
        // that report no single-tap capability (e.g. MX Master 4) — lacking the
        // tap only means a bound click can never fire.
        if wheel_info.is_some_and(|info| !info.supports_single_tap) {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
        // Store ownership before the write: a transport error cannot prove
        // whether firmware applied diversion, so rollback must cover it too.
        armed.thumb = Some(ArmedThumbwheel {
            wheel: tw,
            info: wheel_info,
        });
        if let Some(thumb) = armed.thumb.as_ref()
            && let Err(error) = thumb.wheel.divert(thumb.direction()).await
        {
            return Err(GestureError::Hidpp(format!("{error:?}")));
        }
    }

    arm_touchpad(device, chan, slot, spec, touchpad_journal, armed).await?;
    Ok(())
}

async fn arm_touchpad(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    journal: Option<&Arc<dyn TouchpadJournalStore>>,
    armed: &mut ArmedControls,
) -> Result<(), GestureError> {
    let (Some(journal_id), Some(journal)) = (&spec.touchpad_journal_id, journal) else {
        if spec.capture_touchpad {
            warn!(
                "touchpad capture requested without durable device identity/journal — not arming"
            );
        }
        return Ok(());
    };
    let Some(info) = device
        .root()
        .get_feature(hidpp::feature::touchpad_raw_xy::TouchpadRawXyFeature::ID)
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?
    else {
        return Ok(());
    };
    let feature = hidpp::feature::touchpad_raw_xy::TouchpadRawXyFeature::new(
        Arc::clone(chan),
        slot,
        info.index,
    );
    let gestures = match device
        .root()
        .get_feature(hidpp::feature::gestures2::Gestures2Feature::ID)
        .await
    {
        Ok(Some(info)) => Some(hidpp::feature::gestures2::Gestures2Feature::new(
            Arc::clone(chan),
            slot,
            info.index,
        )),
        Ok(None) => None,
        Err(error) => {
            warn!(%error, "0x6501 feature lookup failed — arming without the two-finger divert");
            None
        }
    };
    ArmedRawMode::recover(&feature, gestures.as_ref(), journal.as_ref(), journal_id)
        .await
        .map_err(|error| GestureError::Hidpp(error.to_string()))?;
    if !spec.capture_touchpad {
        return Ok(());
    }
    let touchpad_info = feature
        .get_touchpad_info()
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
    let stream = TouchpadFrameStream::new(touchpad_info)
        .map_err(|error| GestureError::Hidpp(error.to_string()))?;
    let raw_mode = ArmedRawMode::arm(&feature, gestures.as_ref(), journal.as_ref(), journal_id)
        .await
        .map_err(touchpad_capture_error)?;
    armed.touchpad = Some(ArmedTouchpad {
        feature,
        stream,
        raw_mode,
        gestures,
        journal: Arc::clone(journal),
        journal_id: journal_id.clone(),
    });
    Ok(())
}

fn touchpad_capture_error(error: TouchpadCaptureError) -> GestureError {
    match error {
        TouchpadCaptureError::ExternalRawMode { actual } => GestureError::TouchpadRawModeConflict {
            expected: OPENLOGI_RAW_REPORT_FLAGS.bits(),
            actual,
        },
        error => GestureError::Hidpp(error.to_string()),
    }
}

fn send_touchpad_event(event: TouchpadStreamEvent, sink: &mpsc::UnboundedSender<CapturedInput>) {
    let input = match event {
        TouchpadStreamEvent::Frame(frame) => CapturedInput::TouchpadFrame(frame),
        TouchpadStreamEvent::End => CapturedInput::TouchpadEnd,
        TouchpadStreamEvent::Cancel => CapturedInput::TouchpadCancel,
        TouchpadStreamEvent::DroppedFrames(count) => CapturedInput::TouchpadDroppedFrames(count),
    };
    let _ = sink.send(input);
}

async fn arm_reprog_control(
    rc: &ReprogControlsV4,
    cid: u16,
    raw_xy: bool,
    reporting: &mut Vec<ArmedReporting>,
) -> Result<(), GestureError> {
    let original = rc
        .get_cid_reporting(cid)
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
    if original.diverted {
        // Left over from a session that never tore down (agent killed, or
        // another Logitech app). Worth a line: it is the state that used to be
        // replayed on restore, leaving the button dead.
        debug!(cid, "control was already diverted before arming");
    }
    let change = divert_change(original, raw_xy);
    // Record ownership before the write: a transport error does not prove the
    // firmware rejected the command, so rollback must cover this CID too.
    reporting.push(ArmedReporting { cid, original });
    rc.set_cid_reporting_full(cid, change)
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
    Ok(())
}

/// The [`ButtonId`] a gesture-source CID dispatches as, per
/// [`GESTURE_SOURCE_BUTTONS`]; `None` for a CID that is not a gesture source.
/// A spec listing an unknown CID therefore never begins a hold — the press is
/// dropped rather than misattributed.
fn gesture_source_button(cid: u16) -> Option<ButtonId> {
    GESTURE_SOURCE_BUTTONS
        .into_iter()
        .find(|&(c, _)| c == cid)
        .map(|(_, button)| button)
}

fn captured_gesture_button(cid: u16, gesture_button_cids: &[(u16, ButtonId)]) -> Option<ButtonId> {
    gesture_source_button(cid).or_else(|| {
        gesture_button_cids
            .iter()
            .find(|&&(candidate, _)| candidate == cid)
            .map(|&(_, button)| button)
    })
}

/// Read the device's full reprogrammable-control table in one pass, so we can
/// test several CIDs without rescanning per control.
pub(crate) async fn enumerate_controls(
    rc: &ReprogControlsV4,
) -> Result<Vec<reprog_controls::CtrlIdInfo>, GestureError> {
    let count = rc
        .get_count()
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
    let mut controls = Vec::with_capacity(usize::from(count));
    for index in 0..count {
        controls.push(
            rc.get_ctrl_id_info(index)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?,
        );
    }
    Ok(controls)
}

/// Update `acc` and emit on a decoded `0x1b04` event: preserve physical button
/// edges, and commit a gesture swipe the instant it crosses the threshold
/// (mid-swipe, like Options+) rather than on release.
fn handle_reprog_with_gesture_buttons(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_cids: &[u16],
    dpi_cids: &[u16],
    gesture_button_cids: &[(u16, ButtonId)],
    button_cids: &[(u16, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            // The swipe accumulator belongs to the raw-XY gesture diverts.
            // When a gesture-source control is instead diverted as a plain
            // button (a single binding, not gesture mode), its press must flow
            // through the `button_cids` loop only — not also emit a click.
            let held: Vec<(u16, ButtonId)> = gesture_cids
                .iter()
                .filter(|cid| cids.contains(cid))
                .filter_map(|&cid| gesture_source_button(cid).map(|b| (cid, b)))
                .chain(
                    gesture_button_cids
                        .iter()
                        .copied()
                        .filter(|(cid, _)| cids.contains(cid)),
                )
                .collect();
            acc.hold = match std::mem::take(&mut acc.hold) {
                // The holder is still down. While a second armed source is
                // held alongside it, unattributed raw-XY motion is dropped
                // (see [`HoldState::Holding::overlap`]).
                HoldState::Holding {
                    cid,
                    button,
                    swipe,
                    skip_first_raw_xy,
                    ..
                } if cids.contains(&cid) => HoldState::Holding {
                    cid,
                    button,
                    swipe,
                    overlap: held.len() > 1,
                    skip_first_raw_xy,
                },
                previous => {
                    // No holder, or the holder released: a released hold that
                    // never committed a direction is a plain click...
                    if let HoldState::Holding {
                        button, mut swipe, ..
                    } = previous
                        && swipe.end()
                    {
                        debug!(%button, "gesture click");
                        let _ = sink.send(CapturedInput::Gesture(button, GestureDirection::Click));
                    }
                    // ...and the first still-held source begins (or takes
                    // over) the hold. A source not down in the previous event
                    // is a fresh touch, so the panel's contact-jump discard
                    // applies; one that was already held has had its jump
                    // dropped during the overlap.
                    match held.first() {
                        Some(&(cid, button)) => begin_hold(
                            cid,
                            button,
                            held.len() > 1,
                            cid == reprog_controls::HAPTIC_PANEL_CID
                                && !acc.gestures_down.contains(&cid),
                        ),
                        None => HoldState::Idle,
                    }
                }
            };
            // Gesture semantics stay separate from the physical lifecycle:
            // click/swipe remains one completed action, while every armed
            // source also contributes one rising and one falling edge to the
            // shared button runtime.
            for &cid in &acc.gestures_down {
                if !held.iter().any(|(held_cid, _)| *held_cid == cid)
                    && let Some(button) = captured_gesture_button(cid, gesture_button_cids)
                {
                    let _ = sink.send(CapturedInput::ButtonUp(button));
                }
            }
            for &(cid, button) in &held {
                if !acc.gestures_down.contains(&cid) {
                    let _ = sink.send(CapturedInput::ButtonDown(button));
                }
            }
            acc.gestures_down = held.into_iter().map(|(cid, _)| cid).collect();

            let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
            if dpi_down && !acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonDown(ButtonId::DpiToggle));
            } else if !dpi_down && acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonUp(ButtonId::DpiToggle));
            }
            acc.dpi_down = dpi_down;

            for &(cid, button) in button_cids {
                let down = cids.contains(&cid);
                let was_down = acc.buttons_down.contains(&cid);
                if down && !was_down {
                    let _ = sink.send(CapturedInput::ButtonDown(button));
                    acc.buttons_down.push(cid);
                } else if !down && was_down {
                    let _ = sink.send(CapturedInput::ButtonUp(button));
                    acc.buttons_down.retain(|&c| c != cid);
                }
            }
        }
        RawControlEvent::RawXy { dx, dy } => {
            handle_raw_xy(acc, dx, dy, sink);
        }
    }
}

fn handle_raw_xy(
    acc: &mut CaptureAccum,
    dx: i16,
    dy: i16,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    // Motion is attributed to the holding source; outside a hold the report
    // is stray and dropped.
    let HoldState::Holding {
        button,
        swipe,
        overlap,
        skip_first_raw_xy,
        ..
    } = &mut acc.hold
    else {
        return;
    };
    // While two armed sources are held the report could belong to either
    // control — drop it rather than miscommit a swipe through the holder's map.
    if *overlap {
        return;
    }
    // The haptic panel's first sample after contact is a position jump;
    // summing it would commit a bogus direction instantly.
    if *skip_first_raw_xy {
        *skip_first_raw_xy = false;
        return;
    }
    // Commit the instant a clean direction emerges (mid-swipe, once per hold);
    // the accumulator gates on hold duration internally and drops travel that
    // arrives outside a hold.
    if let Some(direction) = swipe.accumulate(i32::from(dx), i32::from(dy)) {
        debug!(?direction, %button, "gesture committed");
        let _ = sink.send(CapturedInput::Gesture(*button, direction));
    }
}

/// Test seam for the pre-existing raw-XY/plain-button cases, none of which
/// carries a standard-button gesture hold.
#[cfg(test)]
fn handle_reprog(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_cids: &[u16],
    dpi_cids: &[u16],
    button_cids: &[(u16, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    handle_reprog_with_gesture_buttons(acc, event, gesture_cids, dpi_cids, &[], button_cids, sink);
}
#[cfg(test)]
mod tests;

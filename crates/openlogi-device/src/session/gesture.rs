//! Live control capture for one device: divert the device's gesture sources
//! (the MX dedicated gesture button and/or the MX Master 4 haptic panel), the
//! DPI/ModeShift button, and the thumb wheel over HID++ and turn their events
//! into [`CapturedInput`] the GUI can dispatch.
//!
//! [`run_capture_session`] holds a single HID++ channel open for one device,
//! enables diversion on whichever of those controls it exposes, registers one
//! message listener, and restores every control's default mapping on shutdown.
//! Using one channel matters: a second channel to the same device would split
//! its input-report stream, so all captured controls share this session.
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! The agent maps each [`CapturedInput`] to the user's bound action and dispatches
//! it, mirroring how the CGEventTap hook handles the side buttons. The thumb
//! wheel is special: diverting it stops native horizontal scroll, so the agent
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when the user's thumbwheel config leaves its
//! defaults (click bound, rotation rebound, or sensitivity changed). While
//! diverted, the transport forwards every report; the agent applies the current
//! app/profile policy before dispatching a tap.

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hidpp::{channel::HidppChannel, device::Device, protocol::v20};
use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::SharedChannel;
use crate::backend::{BackendError, HidBackend};
use crate::channel::route::{DeviceRoute, open_route_channel};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::thumbwheel::{self, Thumbwheel, ThumbwheelEvent};

/// How often the capture session pings its device to prove the channel still
/// delivers input reports. Cheap: one HID++ round-trip per interval.
const LIVENESS_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// Consecutive all-silent pings after which the capture channel is declared
/// dead. Two, so one ping lost to transient receiver congestion (which does
/// happen under pointer load) doesn't churn the session.
const LIVENESS_PING_STRIKES: u8 = 2;

/// Shared slot holding the active capture session's open channel, so DPI /
/// SmartShift writes can reuse it instead of opening a fresh one. `None`
/// whenever no session is connected.
pub type CaptureChannel = Arc<RwLock<Option<SharedChannel>>>;

/// Why a capture session is shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStop {
    /// Normal stop — restore diverted controls.
    Graceful,
    /// Lease revoked / channel dying — skip restore writes.
    Revoked,
}

/// How a capture session handles the thumb wheel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThumbwheelCaptureMode {
    /// Leave the wheel in its native HID reporting mode.
    #[default]
    Native,
    /// Divert the wheel to HID++ and forward all decoded reports to the agent.
    Diverted,
}

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturedInput {
    /// A completed swipe (or tap click) from a diverted gesture source,
    /// tagged with the source control so dispatch resolves it against that
    /// button's own direction map.
    Gesture(ButtonId, GestureDirection),
    /// A diverted button was pressed — the DPI/ModeShift button
    /// ([`ButtonId::DpiToggle`]) or the thumb-wheel single tap
    /// ([`ButtonId::Thumbwheel`]).
    ButtonPressed(ButtonId, #[serde(skip)] Option<i32>),
    /// Thumb-wheel rotation to re-synthesise as horizontal scroll, in the
    /// wheel's `diverted_res` increments. Emitted while the wheel is diverted
    /// (click bound, rotation rebound, or sensitivity changed).
    Scroll(i16),
}

/// Why a capture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum GestureError {
    /// HID transport-level failure while enumerating or opening the device.
    #[error("HID transport error")]
    Hid(#[from] BackendError),
    /// No connected device matched the capture route.
    #[error("no connected device matched the capture route")]
    DeviceNotFound,
    /// The device at the target index did not answer HID++.
    #[error("device at index {0:#04x} did not respond to HID++")]
    DeviceUnreachable(u8),
    /// A HID++ feature call returned an error; inner string carries context.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

/// Movement + button state accumulated across messages. Lives behind a `Mutex`
/// because the channel's read thread invokes the listener by shared reference.
#[derive(Default)]
struct CaptureAccum {
    /// Mid-swipe state for the currently held gesture source (raw-XY).
    swipe: SwipeAccumulator,
    /// The gesture source that began the current hold, with the [`ButtonId`]
    /// its events dispatch as. Raw-XY reports carry no source attribution, so
    /// the first held source owns the accumulated motion until it is released
    /// (first hold wins). While a second source is held alongside it, motion
    /// is dropped instead of miscommitted (see [`Self::overlap`]); when the
    /// holder releases, a still-held source takes the hold over.
    gesture_source: Option<(u16, ButtonId)>,
    /// Whether a second armed source is held alongside the holder. Raw-XY
    /// reports are unattributed on the wire, so overlap motion could belong to
    /// either control — it is dropped until the overlap ends.
    overlap: bool,
    /// The armed gesture sources held in the last event, for edge detection:
    /// a source not previously held that becomes the holder is a fresh touch
    /// (the haptic panel's first sample is then a contact jump to discard).
    gestures_down: Vec<u16>,
    /// Whether the current hold's next raw-XY sample must be dropped: the
    /// haptic panel's first sample after contact is an absolute position
    /// jump, not a delta (see [`reprog_controls::HAPTIC_PANEL_CID`]).
    skip_first_raw_xy: bool,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
    /// Diverted standard-button CIDs held in the last event.
    buttons_down: Vec<u16>,
}

/// HID++-divertable standard buttons: the `0x1b04` control ID and the
/// [`ButtonId`] its press dispatches as. A button is diverted per device only
/// when its binding leaves the default, so an unbound button keeps its native
/// HID behavior (no re-synthesis needed). The Haptic Sense Panel is a gesture
/// source ([`GESTURE_SOURCE_BUTTONS`]), not a member of this table.
pub const DIVERTABLE_STANDARD_BUTTONS: [(u16, ButtonId); 3] = [
    (0x0052, ButtonId::MiddleClick),
    (0x0053, ButtonId::Back),
    (0x0056, ButtonId::Forward),
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

/// Which of one device's controls a capture session should divert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureSpec {
    /// Whether to leave the thumb wheel native or divert it over `0x2150`.
    /// Either diverted mode arms the same physical reporting state; tap intent
    /// is filtered against the latest binding map by the agent.
    pub thumbwheel_mode: ThumbwheelCaptureMode,
    /// Gesture-source CIDs ([`GESTURE_SOURCE_BUTTONS`] members) to divert
    /// with raw-XY — one per source in gesture mode; empty when no HID++
    /// control gestures.
    pub divert_gesture_sources: Vec<u16>,
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
/// Opens and holds one HID++ channel, diverts whichever of those controls the
/// device exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped), after restoring every diverted control. Setup errors are returned;
/// failures to restore on the way out are logged, not propagated.
pub async fn run_capture_session(
    backend: &dyn HidBackend,
    route: DeviceRoute,
    spec: CaptureSpec,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let chan = open_route_channel(backend, &route)
        .await?
        .ok_or(GestureError::DeviceNotFound)?;
    let device_index = route.device_index();
    let armed = arm_controls(&chan, device_index, &spec).await?;

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(SharedChannel::new(Arc::clone(&chan), route.clone()));
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let reprog_index = armed.reprog.as_ref().map(|(_, idx)| *idx);
    let gesture_cids = armed.gesture_cids.clone();
    let thumb_index = armed.thumb.as_ref().map(|(_, idx)| *idx);
    let dpi_set = armed.dpi_cids.clone();
    let button_set = armed.button_cids.clone();
    let listener = chan.add_msg_listener_guarded({
        let accum = Arc::clone(&accum);
        let sink = sink.clone();
        move |raw, matched| {
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
                handle_reprog(&mut acc, event, &gesture_cids, &dpi_set, &button_set, &sink);
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(&msg, device_index, idx)
            {
                forward_thumbwheel_event(event, &sink);
            }
        }
    });

    info!(
        index = device_index,
        gesture_sources = armed.gesture_cids.len(),
        dpi_buttons = armed.dpi_cids.len(),
        buttons = armed.button_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        "control capture active"
    );

    // Liveness watchdog: this session's channel is the sole delivery path for
    // every diverted control, and a channel whose input-report delivery dies
    // (observed on macOS with concurrent opens of one node: writes accepted,
    // replies and events silently routed elsewhere) turns every captured
    // button to dead air with nothing to notice. Ping the device through this
    // channel; consecutive all-silent pings mean the channel — not the device
    // — is gone (a sleeping/unreachable device still gets us an error *reply*,
    // which proves delivery and resets the count). Exiting lets the manager
    // re-arm on a fresh channel.
    let root = <hidpp::feature::root::RootFeature as hidpp::feature::CreatableFeature>::new(
        Arc::clone(&chan),
        device_index,
        0,
    );
    let mut shutdown = std::pin::pin!(shutdown);
    let mut silent_pings = 0u8;
    let channel_dead = loop {
        tokio::select! {
            _ = &mut shutdown => break false,
            () = tokio::time::sleep(LIVENESS_PING_INTERVAL) => {
                match root.ping(0x5a).await {
                    Err(v20::Hidpp20Error::Channel(
                        hidpp::channel::ChannelError::Timeout
                        | hidpp::channel::ChannelError::NoResponse,
                    )) => {
                        silent_pings = silent_pings.saturating_add(1);
                        if silent_pings >= LIVENESS_PING_STRIKES {
                            warn!(
                                index = device_index,
                                "capture channel stopped delivering — restarting session on a fresh channel"
                            );
                            break true;
                        }
                    }
                    // Any reply — pong, feature error, unreachable-device
                    // error — proves the channel still delivers.
                    _ => silent_pings = 0,
                }
            }
        }
    };

    drop(listener);
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
    if channel_dead {
        // Disarm writes would each burn a timeout on a channel that no longer
        // answers, and the replacement session re-arms the same diverts
        // anyway; leave the device state for it.
        debug!(index = device_index, "skipping disarm on a dead channel");
    } else {
        armed.disarm().await;
    }
    debug!(index = device_index, "control capture stopped");
    Ok(())
}

/// Forward every actionable part of a decoded diverted thumb-wheel report.
///
/// Tap intent is deliberately not captured here: app/profile changes update the
/// agent's live policy without restarting this hardware session.
fn forward_thumbwheel_event(event: ThumbwheelEvent, sink: &mpsc::UnboundedSender<CapturedInput>) {
    if event.single_tap {
        let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::Thumbwheel, None));
    }
    if event.rotation != 0 {
        let _ = sink.send(CapturedInput::Scroll(event.rotation));
    }
}

/// Reason-aware capture: maps stop reasons onto a unit oneshot shutdown.
pub async fn run_capture_session_with_stop_reason(
    backend: &dyn HidBackend,
    route: DeviceRoute,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<CaptureStop>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = shutdown.await;
        let _ = tx.send(());
    });
    let spec = CaptureSpec {
        thumbwheel_mode: if capture_thumbwheel {
            ThumbwheelCaptureMode::Diverted
        } else {
            ThumbwheelCaptureMode::Native
        },
        // The bool-era API only ever meant the dedicated gesture button; the
        // haptic panel is reachable through [`CaptureSpec`] itself.
        divert_gesture_sources: divert_gesture_button
            .then_some(reprog_controls::GESTURE_BUTTON_CID)
            .into_iter()
            .collect(),
        divert_buttons: Vec::new(),
    };
    run_capture_session(backend, route, spec, sink, rx, channel_slot).await
}

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
#[derive(Default)]
struct ArmedControls {
    /// `0x1b04` accessor + feature index, present when the device exposes it.
    reprog: Option<(ReprogControlsV4, u8)>,
    /// The gesture-source CIDs diverted with raw-XY reporting: the
    /// `spec.divert_gesture_sources` members the device exposes.
    gesture_cids: Vec<u16>,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// Standard-button CIDs diverted per the session's [`CaptureSpec`], with
    /// the [`ButtonId`] each dispatches as.
    button_cids: Vec<(u16, ButtonId)>,
    /// Original reporting state for every diverted `0x1b04` control.
    reporting: Vec<ArmedCid>,
    /// `0x2150` accessor + feature index, present when the thumb wheel is
    /// diverted.
    thumb: Option<(Thumbwheel, u8)>,
}

#[derive(Clone, Copy)]
struct ArmedCid {
    cid: u16,
    original: reprog_controls::CidReporting,
}

impl ArmedControls {
    /// Restore every diverted control. Failures are logged, not propagated.
    async fn disarm(&self) {
        if let Some((rc, _)) = self.reprog.as_ref() {
            for &reporting in &self.reporting {
                restore_reporting(rc, reporting, "captured control").await;
            }
        }
        if let Some((tw, _)) = self.thumb.as_ref() {
            restore(tw.set_reporting(false, false).await, "thumb wheel");
        }
    }
}

/// Resolve features off the device's root and divert the controls `spec`
/// selects: the gesture sources (raw-XY), DPI/ModeShift buttons and rebindable
/// standard buttons over `0x1b04`, and — when `spec.thumbwheel_mode` is
/// diverted — the thumb wheel over `0x2150`. The
/// root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
///
/// A failure mid-way hands every already-diverted control back to the firmware
/// before returning: with several controls armed one after another, aborting
/// without disarming would leave the earlier ones diverted with no session
/// listening — captured-and-dropped until a later respawn succeeds.
async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
) -> Result<ArmedControls, GestureError> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;
    let mut armed = ArmedControls::default();
    if let Err(error) = arm_controls_into(&device, chan, slot, spec, &mut armed).await {
        armed.disarm().await;
        return Err(error);
    }
    if armed.gesture_cids.is_empty()
        && armed.dpi_cids.is_empty()
        && armed.button_cids.is_empty()
        && armed.thumb.is_none()
    {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(armed)
}

/// The fallible arming steps of [`arm_controls`], recording each successful
/// divert into `armed` as it lands — so the caller can disarm exactly what was
/// armed when a later step fails.
async fn arm_controls_into(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    armed: &mut ArmedControls,
) -> Result<(), GestureError> {
    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let rc = ReprogControlsV4::new(Arc::clone(chan), slot, info.index);
        let controls = enumerate_controls(&rc).await?;
        // Register an accessor before the first divert, so a failure on any
        // divert (including the first) can be handed back via `disarm`.
        armed.reprog = Some((rc.clone(), info.index));

        // Divert each gesture-mode source; a source not listed stays native
        // (an idle HID++ control must not be captured-and-dropped).
        for &cid in &spec.divert_gesture_sources {
            if controls.iter().any(|c| c.cid == cid && c.supports_raw_xy()) {
                let reporting = arm_reprog_control(&rc, cid, true).await?;
                armed.reporting.push(reporting);
                armed.gesture_cids.push(cid);
            }
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                let reporting = arm_reprog_control(&rc, cid, false).await?;
                armed.reporting.push(reporting);
                armed.dpi_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_buttons {
            // The plan never lists a raw-XY-diverted gesture source, but
            // guard anyway: a plain (divert, no raw-XY) write here would strip
            // the raw-XY reporting armed above.
            if armed.gesture_cids.contains(&cid) {
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                let reporting = arm_reprog_control(&rc, cid, false).await?;
                armed.reporting.push(reporting);
                armed.button_cids.push((cid, button));
            }
        }
    }

    if spec.thumbwheel_mode != ThumbwheelCaptureMode::Native
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
        let supports_single_tap = match tw.get_info().await {
            Ok(twinfo) => twinfo.supports_single_tap,
            Err(e) => {
                warn!(error = ?e, "thumb wheel getInfo failed");
                false
            }
        };
        // Divert whenever capture was requested: rotation rebinds and the
        // sensitivity multiplier need the diverted event stream even on wheels
        // that report no single-tap capability (e.g. MX Master 4) — lacking the
        // tap only means a bound click can never fire.
        if !supports_single_tap {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
        if let Err(error) = tw.set_reporting(true, false).await {
            let error = GestureError::Hidpp(format!("{error:?}"));
            restore(
                tw.set_reporting(false, false).await,
                "failed thumb wheel diversion",
            );
            return Err(error);
        }
        armed.thumb = Some((tw, info.index));
    }
    Ok(())
}

async fn arm_reprog_control(
    rc: &ReprogControlsV4,
    cid: u16,
    raw_xy: bool,
) -> Result<ArmedCid, GestureError> {
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
    let mut change = reprog_controls::CidReportingChange::temporary_diversion(true, raw_xy);
    change.remap = original.remap;
    if let Err(error) = rc.set_cid_reporting_full(cid, change).await {
        let error = GestureError::Hidpp(format!("{error:?}"));
        restore_reporting(rc, ArmedCid { cid, original }, "failed diversion").await;
        return Err(error);
    }
    Ok(ArmedCid { cid, original })
}

/// The mirror image of arming: clear the diversion this session turned on and
/// hand the control's remap target back untouched.
///
/// Deliberately *not* a verbatim replay of the snapshot. A control can already
/// be diverted when the session arms it — the agent was killed mid-session, or
/// Logi Options+ left its own diversion behind — and replaying that snapshot
/// hands the button back diverted with nothing listening for its HID++ events
/// and no OS event either: dead until the device sleeps or reconnects, since
/// diversion is volatile. Arming only ever sets `diverted` / `raw_xy` (plus
/// re-asserting `remap`), so undoing exactly those fields is the whole job;
/// every other bit stays `None`, i.e. unchanged.
fn undivert_change(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    let mut change = reprog_controls::CidReportingChange::temporary_diversion(false, false);
    change.remap = reporting.remap;
    change
}

async fn restore_reporting(rc: &ReprogControlsV4, armed: ArmedCid, what: &str) {
    let result = rc
        .set_cid_reporting_full(armed.cid, undivert_change(armed.original))
        .await
        .map(|_| ());
    restore(result, what);
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

/// Log (don't propagate) a failure to hand a control back to the firmware.
pub(crate) fn restore<E: std::fmt::Display>(result: Result<(), E>, what: &str) {
    if let Err(e) = result {
        warn!(error = %e, control = what, "failed to restore control mapping on shutdown");
    }
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

/// Update `acc` and emit on a decoded `0x1b04` event: commit a gesture swipe the
/// instant it crosses the threshold (mid-swipe, like Options+) rather than on
/// release, and emit a [`ButtonId::DpiToggle`] press on the rising edge of any
/// diverted DPI/ModeShift control.
fn handle_reprog(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_cids: &[u16],
    dpi_cids: &[u16],
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
                .collect();
            match acc.gesture_source {
                Some((cid, _)) if cids.contains(&cid) => {
                    // The holder is still down. While a second armed source is
                    // held alongside it, unattributed raw-XY motion is dropped
                    // (see `CaptureAccum::overlap`).
                    acc.overlap = held.len() > 1;
                }
                previous => {
                    // No holder, or the holder released: a released hold that
                    // never committed a direction is a plain click...
                    if let Some((_, button)) = previous {
                        acc.gesture_source = None;
                        acc.overlap = false;
                        if acc.swipe.end() {
                            debug!(%button, "gesture click");
                            let _ =
                                sink.send(CapturedInput::Gesture(button, GestureDirection::Click));
                        }
                    }
                    // ...and the first still-held source begins (or takes
                    // over) the hold. A source not down in the previous event
                    // is a fresh touch, so the panel's contact-jump discard
                    // applies; one that was already held has had its jump
                    // dropped during the overlap.
                    if let Some(&(cid, button)) = held.first() {
                        acc.gesture_source = Some((cid, button));
                        acc.swipe.begin();
                        acc.overlap = held.len() > 1;
                        acc.skip_first_raw_xy = cid == reprog_controls::HAPTIC_PANEL_CID
                            && !acc.gestures_down.contains(&cid);
                    }
                }
            }
            acc.gestures_down = held.into_iter().map(|(cid, _)| cid).collect();

            let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
            if dpi_down && !acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None));
            }
            acc.dpi_down = dpi_down;

            for &(cid, button) in button_cids {
                let down = cids.contains(&cid);
                let was_down = acc.buttons_down.contains(&cid);
                if down && !was_down {
                    let _ = sink.send(CapturedInput::ButtonPressed(button, None));
                    acc.buttons_down.push(cid);
                } else if !down && was_down {
                    acc.buttons_down.retain(|&c| c != cid);
                }
            }
        }
        RawControlEvent::RawXy { dx, dy } => {
            // Motion is attributed to the holding source; outside a hold the
            // report is stray and dropped.
            let Some((_, button)) = acc.gesture_source else {
                return;
            };
            // While two armed sources are held the report could belong to
            // either control — drop it rather than miscommit a swipe through
            // the holder's map.
            if acc.overlap {
                return;
            }
            // The haptic panel's first sample after contact is a position
            // jump; summing it would commit a bogus direction instantly.
            if acc.skip_first_raw_xy {
                acc.skip_first_raw_xy = false;
                return;
            }
            // Commit the instant a clean direction emerges (mid-swipe, once per
            // hold); the accumulator gates on hold duration internally and drops
            // travel that arrives outside a hold.
            if let Some(direction) = acc.swipe.accumulate(i32::from(dx), i32::from(dy)) {
                debug!(?direction, %button, "gesture committed");
                let _ = sink.send(CapturedInput::Gesture(button, direction));
            }
        }
    }
}
#[cfg(test)]
mod tests;

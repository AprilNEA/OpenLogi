//! Live control capture for one device: divert the device's gesture source
//! (the MX dedicated gesture button, or the MX Master 4 haptic panel), the
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
//! The GUI maps each [`CapturedInput`] to the user's bound action and dispatches
//! it, mirroring how the CGEventTap hook handles the side buttons. The thumb
//! wheel is special: diverting it stops native horizontal scroll, so the GUI
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when the user's thumbwheel config leaves its
//! defaults (click bound, rotation rebound, or sensitivity changed).

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hidpp::{channel::HidppChannel, device::Device, protocol::v20};
use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::thumbwheel::{self, Thumbwheel};
use crate::write::SharedChannel;

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

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturedInput {
    /// A completed gesture-button swipe.
    Gesture(GestureDirection),
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
    Hid(#[from] async_hid::HidError),
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
    /// Mid-swipe state for the diverted gesture source (raw-XY).
    swipe: SwipeAccumulator,
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
/// Sense Panel on MX Master 4 (two distinct physical controls). The one that
/// owns the gesture role is diverted with raw-XY; either is plain-diverted
/// like a standard button when its binding leaves the default without the
/// role.
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
    /// The gesture owner's source CID (a [`GESTURE_SOURCE_BUTTONS`] member) to
    /// divert with raw-XY, or `None` when no HID++ control owns the gesture
    /// role.
    pub divert_gesture_source: Option<u16>,
    /// Buttons to divert as plain presses (no raw-XY): the
    /// [`DIVERTABLE_STANDARD_BUTTONS`] and non-owner [`GESTURE_SOURCE_BUTTONS`]
    /// whose binding leaves the default.
    pub divert_buttons: Vec<(u16, ButtonId)>,
}

/// Capture the controls selected by `spec` on `route` until `shutdown`
/// resolves, forwarding each event to `sink`.
///
/// Only the gesture owner's source (`spec.divert_gesture_source`) is diverted
/// with raw-XY. When the user moves the gesture role to an OS-hook button or
/// turns gestures off, the gesture sources keep their native behavior —
/// unless a non-default single binding puts them in `spec.divert_buttons`, in
/// which case they are diverted as plain buttons (the OS hook never sees a
/// gesture-source CID, so this is the binding's only delivery path). The
/// DPI/ModeShift capture and the channel-reuse slot are independent of this.
///
/// Opens and holds one HID++ channel, diverts whichever of those controls the
/// device exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped), after restoring every diverted control. Setup errors are returned;
/// failures to restore on the way out are logged, not propagated.
pub async fn run_capture_session(
    route: DeviceRoute,
    spec: CaptureSpec,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let chan = open_route_channel(&route)
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
    let gesture_cid = armed.gesture_cid;
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
                handle_reprog(&mut acc, event, gesture_cid, &dpi_set, &button_set, &sink);
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(&msg, device_index, idx)
            {
                if event.single_tap {
                    let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::Thumbwheel, None));
                }
                if event.rotation != 0 {
                    let _ = sink.send(CapturedInput::Scroll(event.rotation));
                }
            }
        }
    });

    info!(
        index = device_index,
        gesture = armed.gesture_cid.is_some(),
        dpi_buttons = armed.dpi_cids.len(),
        buttons = armed.button_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        "control capture active"
    );
    let _ = shutdown.await;

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
    armed.disarm().await;
    debug!(index = device_index, "control capture stopped");
    Ok(())
}

/// Reason-aware capture: maps stop reasons onto a unit oneshot shutdown.
pub async fn run_capture_session_with_stop_reason(
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
        capture_thumbwheel,
        // The bool-era API only ever meant the dedicated gesture button; the
        // haptic panel is reachable through [`CaptureSpec`] itself.
        divert_gesture_source: divert_gesture_button.then_some(reprog_controls::GESTURE_BUTTON_CID),
        divert_buttons: Vec::new(),
    };
    run_capture_session(route, spec, sink, rx, channel_slot).await
}

/// Registry-aware capture: currently opens via route (inventory channel reuse TBD).
pub async fn run_capture_session_with_registry(
    route: DeviceRoute,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<CaptureStop>,
    channel_slot: CaptureChannel,
    _registry: &crate::ChannelRegistry,
) -> Result<(), GestureError> {
    run_capture_session_with_stop_reason(
        route,
        capture_thumbwheel,
        divert_gesture_button,
        sink,
        shutdown,
        channel_slot,
    )
    .await
}

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
#[derive(Default)]
struct ArmedControls {
    /// `0x1b04` accessor + feature index, present when the device exposes it.
    reprog: Option<(ReprogControlsV4, u8)>,
    /// The gesture-source CID diverted with raw-XY reporting, when the spec
    /// selected one and the device exposes it.
    gesture_cid: Option<u16>,
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
/// selects: the gesture button (raw-XY), DPI/ModeShift buttons and rebindable
/// standard buttons over `0x1b04`, and the thumb wheel over `0x2150`. The
/// root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
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
    if armed.gesture_cid.is_none()
        && armed.dpi_cids.is_empty()
        && armed.button_cids.is_empty()
        && armed.thumb.is_none()
    {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(armed)
}

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
        armed.reprog = Some((rc.clone(), info.index));

        // Only divert the gesture owner's source; every other gesture source
        // stays native (a non-owner HID++ control must not be
        // captured-and-dropped).
        if let Some(cid) = spec.divert_gesture_source
            && controls.iter().any(|c| c.cid == cid && c.supports_raw_xy())
        {
            let reporting = arm_reprog_control(&rc, cid, true).await?;
            armed.reporting.push(reporting);
            armed.gesture_cid = Some(cid);
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                let reporting = arm_reprog_control(&rc, cid, false).await?;
                armed.reporting.push(reporting);
                armed.dpi_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_buttons {
            // The plan never lists the raw-XY-diverted gesture source, but
            // guard anyway: a plain (divert, no raw-XY) write here would strip
            // the raw-XY reporting armed above.
            if armed.gesture_cid == Some(cid) {
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                let reporting = arm_reprog_control(&rc, cid, false).await?;
                armed.reporting.push(reporting);
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
    let mut change = reprog_controls::CidReportingChange::temporary_diversion(true, raw_xy);
    change.remap = original.remap;
    if let Err(error) = rc.set_cid_reporting_full(cid, change).await {
        let error = GestureError::Hidpp(format!("{error:?}"));
        restore_reporting(rc, ArmedCid { cid, original }, "failed diversion").await;
        return Err(error);
    }
    Ok(ArmedCid { cid, original })
}

fn reporting_change(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(reporting.diverted),
        persistently_diverted: Some(reporting.persistently_diverted),
        force_raw_xy: Some(reporting.force_raw_xy),
        raw_xy: Some(reporting.raw_xy),
        remap: reporting.remap,
        analytics_key_events: Some(reporting.analytics_key_events),
        raw_wheel: Some(reporting.raw_wheel),
    }
}

async fn restore_reporting(rc: &ReprogControlsV4, armed: ArmedCid, what: &str) {
    let result = rc
        .set_cid_reporting_full(armed.cid, reporting_change(armed.original))
        .await
        .map(|_| ());
    restore(result, what);
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
    gesture_cid: Option<u16>,
    dpi_cids: &[u16],
    button_cids: &[(u16, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            // The swipe accumulator belongs to the raw-XY gesture divert. When
            // a gesture-source control is instead diverted as a plain button
            // (it has a single binding but not the gesture role), its press
            // must flow through the `button_cids` loop only — not also emit a
            // click.
            let owner_held = gesture_cid.is_some_and(|cid| cids.contains(&cid));
            if owner_held {
                if !acc.swipe.is_holding() {
                    acc.swipe.begin();
                    acc.skip_first_raw_xy = gesture_cid == Some(reprog_controls::HAPTIC_PANEL_CID);
                }
            } else if acc.swipe.is_holding() {
                // A press that never committed a direction is a plain click.
                if acc.swipe.end() {
                    debug!("gesture click");
                    let _ = sink.send(CapturedInput::Gesture(GestureDirection::Click));
                }
            }

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
                debug!(?direction, "gesture committed");
                let _ = sink.send(CapturedInput::Gesture(direction));
            }
        }
    }
}
#[cfg(test)]
mod tests;

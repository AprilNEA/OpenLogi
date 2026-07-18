//! Live control capture for one device: divert the MX dedicated gesture button, the
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
//! is therefore only diverted when its click is actually bound.

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

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturedInput {
    /// A completed gesture-button swipe.
    Gesture(GestureDirection),
    /// A diverted button was pressed — the DPI/ModeShift button
    /// ([`ButtonId::DpiToggle`]) or the thumb-wheel single tap
    /// ([`ButtonId::Thumbwheel`]).
    ButtonPressed(ButtonId),
    /// Thumb-wheel rotation to re-synthesise as horizontal scroll, in the
    /// wheel's `diverted_res` increments. Emitted only while the wheel is
    /// diverted to capture its click.
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
    /// Mid-swipe state for the diverted dedicated gesture button (raw-XY).
    swipe: SwipeAccumulator,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
}

/// Capture the gesture button, DPI/ModeShift button, and (when
/// `capture_thumbwheel`) the thumb wheel on `route` until `shutdown` resolves,
/// forwarding each event to `sink`.
///
/// The dedicated gesture button (raw-XY) is diverted only when `divert_gesture_button` —
/// i.e. it is the device's gesture owner. When the user moves the gesture role
/// to an OS-hook button or turns gestures off, the HID++ gesture control is
/// left undiverted so it keeps its native behavior instead of being
/// captured-and-swallowed. The DPI/ModeShift capture and the channel-reuse slot
/// are independent of this.
///
/// Opens and holds one HID++ channel, diverts whichever of those controls the
/// device exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped), after restoring every diverted control. Setup errors are returned;
/// failures to restore on the way out are logged, not propagated.
pub async fn run_capture_session(
    route: DeviceRoute,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let chan = open_route_channel(&route)
        .await?
        .ok_or(GestureError::DeviceNotFound)?;
    let device_index = route.device_index();
    let armed = arm_controls(
        &chan,
        device_index,
        capture_thumbwheel,
        divert_gesture_button,
    )
    .await?;

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(SharedChannel::new(Arc::clone(&chan), route.clone()));
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let reprog_index = armed.reprog.as_ref().map(|(_, idx)| *idx);
    let thumb_index = armed.thumb.as_ref().map(|(_, idx)| *idx);
    let dpi_set = armed.dpi_cids.clone();
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
                handle_reprog(&mut acc, event, &dpi_set, &sink);
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(&msg, device_index, idx)
            {
                if event.single_tap {
                    let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::Thumbwheel));
                }
                if event.rotation != 0 {
                    let _ = sink.send(CapturedInput::Scroll(event.rotation));
                }
            }
        }
    });

    info!(
        index = device_index,
        gesture = armed.gesture_diverted,
        dpi_buttons = armed.dpi_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        "control capture active"
    );
    let _ = shutdown.await;

    drop(listener);
    if let Ok(mut slot) = channel_slot.write() {
        *slot = None;
    }
    armed.disarm().await;
    debug!(index = device_index, "control capture stopped");
    Ok(())
}

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
struct ArmedControls {
    /// `0x1b04` accessor + feature index, present when the device exposes it.
    reprog: Option<(ReprogControlsV4, u8)>,
    /// Whether the gesture button is diverted with raw-XY reporting.
    gesture_diverted: bool,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// `0x2150` accessor + feature index, present when the thumb wheel is
    /// diverted.
    thumb: Option<(Thumbwheel, u8)>,
}

impl ArmedControls {
    /// Restore every diverted control. Failures are logged, not propagated.
    async fn disarm(&self) {
        if let Some((rc, _)) = self.reprog.as_ref() {
            if self.gesture_diverted {
                let r = rc
                    .set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, false, false)
                    .await;
                restore(r, "gesture button");
            }
            for &cid in &self.dpi_cids {
                restore(rc.set_cid_reporting(cid, false, false).await, "DPI button");
            }
        }
        if let Some((tw, _)) = self.thumb.as_ref() {
            restore(tw.set_reporting(false, false).await, "thumb wheel");
        }
    }
}

/// Resolve features off the device's root and divert the controls we capture:
/// the gesture button (raw-XY) and DPI/ModeShift buttons over `0x1b04`, and —
/// when `capture_thumbwheel` and the wheel reports a single tap — the thumb
/// wheel over `0x2150`. The root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
) -> Result<ArmedControls, GestureError> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;

    let mut reprog: Option<(ReprogControlsV4, u8)> = None;
    let mut gesture_diverted = false;
    let mut dpi_cids: Vec<u16> = Vec::new();
    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let rc = ReprogControlsV4::new(Arc::clone(chan), slot, info.index);
        let controls = enumerate_controls(&rc).await?;

        // Only divert the gesture button when it owns the gesture role; otherwise
        // leave it native (a non-owner HID++ control must not be captured-and-dropped).
        if divert_gesture_button
            && controls
                .iter()
                .any(|c| c.cid == reprog_controls::GESTURE_BUTTON_CID && c.supports_raw_xy())
        {
            rc.set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, true, true)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            gesture_diverted = true;
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                rc.set_cid_reporting(cid, true, false)
                    .await
                    .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
                dpi_cids.push(cid);
            }
        }
        reprog = Some((rc, info.index));
    }

    let mut thumb: Option<(Thumbwheel, u8)> = None;
    if capture_thumbwheel
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
        if supports_single_tap {
            tw.set_reporting(true, false)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            thumb = Some((tw, info.index));
        } else {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
    }

    if !gesture_diverted && dpi_cids.is_empty() && thumb.is_none() {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(ArmedControls {
        reprog,
        gesture_diverted,
        dpi_cids,
        thumb,
    })
}

/// Log (don't propagate) a failure to hand a control back to the firmware.
/// Shared with the keyboard capture session (`crate::keyboard`).
pub(crate) fn restore<E: std::fmt::Display>(result: Result<(), E>, what: &str) {
    if let Err(e) = result {
        warn!(error = %e, control = what, "failed to restore control mapping on shutdown");
    }
}

/// Read the device's full reprogrammable-control table in one pass, so we can
/// test several CIDs without rescanning per control. Shared with the keyboard
/// capture session (`crate::keyboard`).
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
    dpi_cids: &[u16],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            let gesture_held = cids.contains(&reprog_controls::GESTURE_BUTTON_CID);
            if gesture_held && !acc.swipe.is_holding() {
                acc.swipe.begin();
            } else if !gesture_held && acc.swipe.is_holding() {
                // A press that never committed a direction is a plain click.
                if acc.swipe.end() {
                    debug!("gesture click");
                    let _ = sink.send(CapturedInput::Gesture(GestureDirection::Click));
                }
            }

            let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
            if dpi_down && !acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::DpiToggle));
            }
            acc.dpi_down = dpi_down;
        }
        RawControlEvent::RawXy { dx, dy } => {
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

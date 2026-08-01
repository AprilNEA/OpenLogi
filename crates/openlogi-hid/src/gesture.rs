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
    /// ([`ButtonId::DpiToggle`]), the thumb-wheel single tap
    /// ([`ButtonId::Thumbwheel`]), or a thumb-side button diverted for hold
    /// tracking (see [`reprog_controls::hold_cid_for_button`]).
    ButtonPressed(ButtonId),
    /// A button diverted for hold tracking was released.
    ///
    /// Emitted only for the buttons named in `hold_buttons`; the DPI/ModeShift
    /// and thumb-wheel captures are rising-edge only and never produce this.
    /// Pairs with [`CapturedInput::ButtonPressed`] so a consumer can measure how
    /// long the button was held — the whole reason those buttons get diverted.
    ButtonReleased(ButtonId),
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
    /// Which hold-tracked CIDs were held in the last event. Both edges matter
    /// here (unlike `dpi_down`), so the consumer can time the hold.
    held: Vec<u16>,
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
    hold_buttons: &[ButtonId],
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
        hold_buttons,
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
    let hold_set = armed.hold.clone();
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
                handle_reprog(&mut acc, event, &dpi_set, &hold_set, &sink);
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
///
/// [`Default`] is what makes partial arming recoverable: [`arm_controls`] starts
/// from an empty record and fills it in as it goes, so a failure half-way still
/// has something to restore.
///
/// The record is deliberately pessimistic — a control is listed *before* its
/// divert is awaited, because the request leaves for the device before the
/// response comes back, so a timed-out or lost response can still leave the
/// control diverted. Restoring one that was never actually diverted just hands
/// back a mapping the firmware already owns; missing one leaves a thumb button
/// dead to the OS.
#[derive(Default)]
struct ArmedControls {
    /// `0x1b04` accessor + feature index, present when the device exposes it.
    reprog: Option<(ReprogControlsV4, u8)>,
    /// Whether the gesture button is diverted with raw-XY reporting.
    gesture_diverted: bool,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// CIDs diverted for hold tracking, paired with the button they represent.
    hold: Vec<(u16, ButtonId)>,
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
            // Undiverting these matters more than the rest: they are the user's
            // ordinary thumb buttons, and leaving them diverted makes them dead
            // to the OS until the mouse is power-cycled.
            for &(cid, _) in &self.hold {
                restore(
                    rc.set_cid_reporting(cid, false, false).await,
                    "hold-tracked button",
                );
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
    hold_buttons: &[ButtonId],
) -> Result<ArmedControls, GestureError> {
    // Arming is several independent HID++ writes and any one of them can fail.
    // Build the record in place so a failure part-way can hand back whatever may
    // already be diverted: without this, a control diverted before the failure
    // stays captured with no session alive to release it — for a thumb button
    // that means it is dead to the OS until the mouse is power-cycled.
    let mut armed = ArmedControls::default();
    match arm_into(
        &mut armed,
        chan,
        slot,
        capture_thumbwheel,
        divert_gesture_button,
        hold_buttons,
    )
    .await
    {
        Ok(()) => Ok(armed),
        Err(e) => {
            warn!(error = %e, "arming failed part-way — restoring the controls already diverted");
            armed.disarm().await;
            Err(e)
        }
    }
}

/// Body of [`arm_controls`], recording each control on `armed` before its divert
/// is awaited — see [`ArmedControls`] for why that ordering is the safe one.
async fn arm_into(
    armed: &mut ArmedControls,
    chan: &Arc<HidppChannel>,
    slot: u8,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
    hold_buttons: &[ButtonId],
) -> Result<(), GestureError> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;

    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        // Recorded before the first divert, not after the last: `disarm` walks
        // this accessor, so it has to be in place for any restore to happen.
        armed.reprog = Some((
            ReprogControlsV4::new(Arc::clone(chan), slot, info.index),
            info.index,
        ));
        let Some((rc, _)) = armed.reprog.as_ref() else {
            unreachable!("just assigned");
        };
        let controls = enumerate_controls(rc).await?;

        // Only divert the gesture button when it owns the gesture role; otherwise
        // leave it native (a non-owner HID++ control must not be captured-and-dropped).
        if divert_gesture_button
            && controls
                .iter()
                .any(|c| c.cid == reprog_controls::GESTURE_BUTTON_CID && c.supports_raw_xy())
        {
            armed.gesture_diverted = true;
            rc.set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, true, true)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                armed.dpi_cids.push(cid);
                rc.set_cid_reporting(cid, true, false)
                    .await
                    .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            }
        }
        // Thumb-side buttons, diverted only when a caller asked for hold
        // tracking. A button the firmware does not mark divertable is skipped
        // rather than forced: capturing a control the device won't hand over
        // would swallow the press with nothing to show for it.
        for &button in hold_buttons {
            let Some(cid) = reprog_controls::hold_cid_for_button(button) else {
                continue;
            };
            if armed.hold.iter().any(|(existing, _)| *existing == cid) {
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                armed.hold.push((cid, button));
                rc.set_cid_reporting(cid, true, false)
                    .await
                    .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            } else {
                debug!(
                    ?button,
                    cid, "button not divertable — hold tracking skipped"
                );
            }
        }
    }

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
            armed.thumb = Some((tw, info.index));
            let Some((tw, _)) = armed.thumb.as_ref() else {
                unreachable!("just assigned");
            };
            tw.set_reporting(true, false)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
        } else {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
    }

    if !armed.gesture_diverted
        && armed.dpi_cids.is_empty()
        && armed.hold.is_empty()
        && armed.thumb.is_none()
    {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(())
}

/// Log (don't propagate) a failure to hand a control back to the firmware.
fn restore<E: std::fmt::Display>(result: Result<(), E>, what: &str) {
    if let Err(e) = result {
        warn!(error = %e, control = what, "failed to restore control mapping on shutdown");
    }
}

/// Read the device's full reprogrammable-control table in one pass, so we can
/// test several CIDs without rescanning per control.
async fn enumerate_controls(
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
    hold: &[(u16, ButtonId)],
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

            // Hold-tracked buttons report both edges. The event carries the
            // *complete* set of controls currently held, so a CID that was in
            // the last set and is missing now is a release — the device sends
            // one report per change, not one per poll.
            for &(cid, button) in hold {
                let now = cids.contains(&cid);
                let was = acc.held.contains(&cid);
                if now && !was {
                    acc.held.push(cid);
                    let _ = sink.send(CapturedInput::ButtonPressed(button));
                } else if !now && was {
                    acc.held.retain(|held| *held != cid);
                    let _ = sink.send(CapturedInput::ButtonReleased(button));
                }
            }
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

//! Live control capture for one device: divert the MX thumb gesture button, the
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
use std::time::{Duration, Instant};

use hidpp::{channel::HidppChannel, device::Device, protocol::v20};
use openlogi_core::binding::{ButtonId, GestureDirection, detect_swipe};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::thumbwheel::{self, Thumbwheel};
use crate::transport::HIDPP_BLE_USAGE_PAGE;
use crate::write::SharedChannel;

/// Shared slot holding the active capture session's open channel, so DPI /
/// SmartShift writes can reuse it instead of opening a fresh one. `None`
/// whenever no session is connected.
pub type CaptureChannel = Arc<RwLock<Option<SharedChannel>>>;

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Bluetooth-direct (0xff43) devices cannot hold a persistent HID++ capture
    /// session without degrading BLE pointer movement.  Transient writes (DPI,
    /// SmartShift, battery) are unaffected.
    #[error("BLE-direct device: persistent capture skipped to prevent pointer movement glitch")]
    BleDirectUnsupported,
}

/// Minimum hold before raw-XY travel counts as a swipe. A quicker tap stays a
/// plain click even if the cursor was moving when it fired.
const GESTURE_HOLD_FOR_SWIPE: Duration = Duration::from_millis(160);

/// Movement + button state accumulated across messages. Lives behind a `Mutex`
/// because the channel's read thread invokes the listener by shared reference.
#[derive(Default)]
struct CaptureAccum {
    /// Whether the gesture button is currently held.
    gesture_held: bool,
    /// When the current hold began — gates swipe vs. click by duration.
    held_since: Option<Instant>,
    /// Accumulated raw-XY travel since the press began.
    dx: i32,
    dy: i32,
    /// Whether a directional swipe has already committed during this hold.
    /// Gestures fire mid-swipe (like Options+) and once per press.
    fired: bool,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
}

/// Capture the gesture button, DPI/ModeShift button, and (when
/// `capture_thumbwheel`) the thumb wheel on `route` until `shutdown` resolves,
/// forwarding each event to `sink`.
///
/// Opens and holds one HID++ channel, diverts whichever of those controls the
/// device exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped), after restoring every diverted control. Setup errors are returned;
/// failures to restore on the way out are logged, not propagated.
pub async fn run_capture_session(
    route: DeviceRoute,
    capture_thumbwheel: bool,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let (hid_info, chan) = open_route_channel(&route)
        .await?
        .ok_or(GestureError::DeviceNotFound)?;

    // Holding the `0xff43` HID interface open persistently degrades BLE pointer
    // movement — jitter that worsens over time — on Bluetooth-direct Logitech
    // mice (MX Master 3, etc.). A second IOHIDDevice open/activate on the shared
    // BLE link appears to trigger connection-parameter renegotiation; transient
    // opens (DPI/SmartShift writes, battery reads) are unaffected. Until a
    // gentler transport lands (a CoreBluetooth GATT path, like Options+), skip
    // the capture session for BLE-direct devices so movement stays smooth. Side
    // buttons still remap via the CGEventTap; Bolt/USB keep full capture.
    if hid_info.usage_page == HIDPP_BLE_USAGE_PAGE {
        info!(
            route = %route,
            "BLE-direct device: skipping persistent capture to prevent pointer glitch \
             (gesture/DPI-button/thumbwheel capture unavailable on BLE-direct)"
        );
        return Err(GestureError::BleDirectUnsupported);
    }

    let device_index = route.device_index();
    let armed = arm_controls(&chan, device_index, capture_thumbwheel).await?;

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(SharedChannel::new(Arc::clone(&chan), route.clone()));
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let reprog_index = armed.reprog.as_ref().map(|(_, idx)| *idx);
    let thumb_index = armed.thumb.as_ref().map(|(_, idx)| *idx);
    let dpi_set = armed.dpi_cids.clone();
    let hdl = chan.add_msg_listener({
        let accum = Arc::clone(&accum);
        let sink = sink.clone();
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            if let Some(idx) = reprog_index {
                if let Some(event) = reprog_controls::decode_event(&msg, device_index, idx) {
                    // Recover the guard even if a prior holder panicked — the
                    // critical section is panic-free, so the data is consistent.
                    let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
                    handle_reprog(&mut acc, event, &dpi_set, &sink);
                    return;
                }
            }
            if let Some(idx) = thumb_index {
                if let Some(event) = thumbwheel::decode_event(&msg, device_index, idx) {
                    if event.single_tap {
                        let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::Thumbwheel));
                    }
                    if event.rotation != 0 {
                        let _ = sink.send(CapturedInput::Scroll(event.rotation));
                    }
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

    chan.remove_msg_listener(hdl);
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

        if controls
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
    if capture_thumbwheel {
        if let Some(info) = device
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
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            let gesture_held = cids.contains(&reprog_controls::GESTURE_BUTTON_CID);
            if gesture_held && !acc.gesture_held {
                acc.gesture_held = true;
                acc.held_since = Some(Instant::now());
                acc.dx = 0;
                acc.dy = 0;
                acc.fired = false;
            } else if !gesture_held && acc.gesture_held {
                acc.gesture_held = false;
                acc.held_since = None;
                // A press that never committed a direction is a plain click.
                if !acc.fired {
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
            // Accumulate until a clean direction commits, then fire immediately
            // and ignore the rest of the hold (one gesture per press).
            if acc.gesture_held && !acc.fired {
                acc.dx = acc.dx.saturating_add(i32::from(dx));
                acc.dy = acc.dy.saturating_add(i32::from(dy));
                // Held long enough to be a deliberate gesture, not a tap whose
                // cursor happened to be in motion? Only then does travel commit.
                let held = acc
                    .held_since
                    .is_some_and(|t| t.elapsed() >= GESTURE_HOLD_FOR_SWIPE);
                if held && let Some(direction) = detect_swipe(acc.dx, acc.dy) {
                    debug!(?direction, dx = acc.dx, dy = acc.dy, "gesture committed");
                    acc.fired = true;
                    let _ = sink.send(CapturedInput::Gesture(direction));
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn press() -> RawControlEvent {
        RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
    }

    fn release() -> RawControlEvent {
        RawControlEvent::DivertedButtons([0, 0, 0, 0])
    }

    #[test]
    fn quick_tap_is_a_click_even_while_the_cursor_moves() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut acc = CaptureAccum::default();

        handle_reprog(&mut acc, press(), &[], &tx);
        handle_reprog(
            &mut acc,
            RawControlEvent::RawXy { dx: 120, dy: 5 },
            &[],
            &tx,
        );
        handle_reprog(&mut acc, release(), &[], &tx);

        assert_eq!(
            rx.try_recv(),
            Ok(CapturedInput::Gesture(GestureDirection::Click))
        );
        assert!(
            rx.try_recv().is_err(),
            "a quick tap emits exactly one click"
        );
    }

    #[test]
    fn a_held_gesture_commits_a_swipe_and_does_not_also_click() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut acc = CaptureAccum::default();

        handle_reprog(&mut acc, press(), &[], &tx);
        // Pretend the button has been held well past the swipe gate.
        acc.held_since = Instant::now().checked_sub(GESTURE_HOLD_FOR_SWIPE * 2);
        handle_reprog(
            &mut acc,
            RawControlEvent::RawXy { dx: 120, dy: 5 },
            &[],
            &tx,
        );

        assert_eq!(
            rx.try_recv(),
            Ok(CapturedInput::Gesture(GestureDirection::Right))
        );

        handle_reprog(&mut acc, release(), &[], &tx);
        assert!(
            rx.try_recv().is_err(),
            "a committed swipe must not also click on release"
        );
    }

    #[test]
    fn a_held_dpi_button_presses_once_on_the_rising_edge() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut acc = CaptureAccum::default();
        let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
        let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

        handle_reprog(&mut acc, down, &[dpi], &tx);
        handle_reprog(&mut acc, down, &[dpi], &tx);

        assert_eq!(
            rx.try_recv(),
            Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
        );
        assert!(rx.try_recv().is_err(), "a held DPI button presses once");
    }
}

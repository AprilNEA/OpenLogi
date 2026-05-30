//! Live gesture-button capture: divert the MX thumb gesture button over HID++
//! `0x1b04` and turn its raw-XY swipes into [`GestureDirection`] events.
//!
//! [`run_gesture_session`] holds a HID++ channel open for one device, enables
//! diversion + raw-XY reporting on the gesture control, and registers a message
//! listener that accumulates movement between the button's press and release.
//! On release it classifies the travel into a direction and forwards it over an
//! mpsc sink; on shutdown it restores the control's default mapping.
//!
//! The session is transport-only — it has no opinion on what a direction *does*.
//! The GUI maps each [`GestureDirection`] to the user's bound action and
//! dispatches it, mirroring how the CGEventTap hook handles the side buttons.

use std::sync::{Arc, Mutex, PoisonError};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    protocol::v20,
    receiver::{self, Receiver},
};
use openlogi_core::binding::{GestureDirection, classify_gesture};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// Minimum accumulated raw-XY travel (device units) before a hold counts as a
/// directional swipe instead of a [`GestureDirection::Click`]. Raw resolution
/// is device-specific, so this is a starting point to tune on real hardware.
const MIN_TRAVEL: u32 = 50;

/// Which device's gesture button to capture. Mirrors how DPI / SmartShift
/// writes target a device: an optional Bolt receiver UID plus a pairing slot.
#[derive(Debug, Clone)]
pub struct GestureTarget {
    /// Bolt receiver unique ID, or `None` to use the first Bolt receiver found.
    pub receiver_uid: Option<String>,
    /// Pairing slot of the device on that receiver.
    pub slot: u8,
}

/// Why a gesture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum GestureError {
    /// HID transport-level failure while enumerating or opening the device.
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
    /// No Bolt receiver matched the target's `receiver_uid`.
    #[error("no matching receiver for the gesture target")]
    ReceiverNotFound,
    /// The device on the target slot did not answer HID++.
    #[error("device on slot {0} did not respond to HID++")]
    DeviceUnreachable(u8),
    /// The device does not expose `ReprogControlsV4` (`0x1b04`).
    #[error("device does not expose ReprogControlsV4 (0x1b04)")]
    Unsupported,
    /// The device has no raw-XY-capable gesture button to capture.
    #[error("device has no raw-XY gesture button")]
    NoGestureButton,
    /// A HID++ feature call returned an error; inner string carries context.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

/// Movement accumulated between a gesture-button press and release. Lives behind
/// a `Mutex` because the channel's read thread invokes the listener by shared
/// reference.
#[derive(Default)]
struct GestureAccum {
    held: bool,
    dx: i32,
    dy: i32,
}

/// Capture the gesture button on `target` until `shutdown` resolves, forwarding
/// each completed swipe to `sink`.
///
/// Opens and holds a HID++ channel, diverts the gesture control with raw-XY
/// reporting, and listens for the resulting events. Returns once `shutdown`
/// fires (or its sender is dropped), after restoring the control's default
/// mapping. Errors before the listen loop starts are returned; a failure to
/// restore the mapping on the way out is logged, not propagated.
pub async fn run_gesture_session(
    target: GestureTarget,
    sink: mpsc::UnboundedSender<GestureDirection>,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), GestureError> {
    let chan = open_target_channel(&target).await?;

    // Resolve the 0x1b04 feature index directly off the root feature — the same
    // bypass `write::open_feature` uses, because hidpp 0.2's central registry
    // doesn't register the features OpenLogi reimplements.
    let device = Device::new(Arc::clone(&chan), target.slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(target.slot))?;
    let info = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
        .ok_or(GestureError::Unsupported)?;
    let reprog = ReprogControlsV4::new(Arc::clone(&chan), target.slot, info.index);

    let control = reprog
        .find_control(reprog_controls::GESTURE_BUTTON_CID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
        .ok_or(GestureError::NoGestureButton)?;
    if !control.supports_raw_xy() {
        return Err(GestureError::NoGestureButton);
    }

    reprog
        .set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, true, true)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;

    let accum = Arc::new(Mutex::new(GestureAccum::default()));
    let device_index = target.slot;
    let feature_index = info.index;
    let hdl = chan.add_msg_listener({
        let accum = Arc::clone(&accum);
        let sink = sink.clone();
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            let Some(event) = reprog_controls::decode_event(&msg, device_index, feature_index)
            else {
                return;
            };
            // Recover the guard even if a prior holder panicked — the critical
            // section is panic-free, so the data is still consistent.
            let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
            match event {
                RawControlEvent::DivertedButtons(_) => {
                    let held = event.is_pressed(reprog_controls::GESTURE_BUTTON_CID);
                    if held && !acc.held {
                        acc.held = true;
                        acc.dx = 0;
                        acc.dy = 0;
                    } else if !held && acc.held {
                        acc.held = false;
                        let direction = classify_gesture(acc.dx, acc.dy, MIN_TRAVEL);
                        debug!(?direction, dx = acc.dx, dy = acc.dy, "gesture released");
                        // Receiver gone => the GUI is tearing down; ignore.
                        let _ = sink.send(direction);
                    }
                }
                RawControlEvent::RawXy { dx, dy } => {
                    if acc.held {
                        acc.dx = acc.dx.saturating_add(i32::from(dx));
                        acc.dy = acc.dy.saturating_add(i32::from(dy));
                    }
                }
            }
        }
    });

    info!(slot = target.slot, "gesture capture active");
    let _ = shutdown.await;

    chan.remove_msg_listener(hdl);
    if let Err(e) = reprog
        .set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, false, false)
        .await
    {
        warn!(error = %e, "failed to restore gesture button mapping on shutdown");
    }
    debug!(slot = target.slot, "gesture capture stopped");
    Ok(())
}

/// Open and return a HID++ channel for `target`, matching the Bolt receiver by
/// UID when one is given. Mirrors `write::with_device`'s selection, but keeps
/// the channel open instead of running a closure and dropping it.
async fn open_target_channel(target: &GestureTarget) -> Result<Arc<HidppChannel>, GestureError> {
    let candidates = enumerate_hidpp_devices().await?;
    for dev in candidates {
        let Some((_, channel)) = open_hidpp_channel(dev).await? else {
            continue;
        };
        let Some(Receiver::Bolt(bolt)) = receiver::detect(Arc::clone(&channel)) else {
            continue;
        };
        if let Some(want) = target.receiver_uid.as_deref() {
            match bolt.get_unique_id().await {
                Ok(uid) if uid.eq_ignore_ascii_case(want) => {}
                _ => continue,
            }
        }
        return Ok(channel);
    }
    Err(GestureError::ReceiverNotFound)
}

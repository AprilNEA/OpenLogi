//! Live key capture for one keyboard: divert the bound F-row controls over
//! HID++ `0x1b04` and turn their presses into [`CapturedInput`] the agent can
//! dispatch.
//!
//! [`run_keyboard_capture_session`] is the keyboard counterpart of
//! [`crate::gesture::run_capture_session`]: one open channel, diversion armed
//! on exactly the controls the caller asks for (an unbound key is never
//! diverted, so it keeps its native firmware function), one message listener,
//! and every diverted control handed back to the firmware on shutdown.
//!
//! Diversion works on the key's *control* — the printed media/shortcut
//! function — so it fires when Fn-lock is off (or via Fn+key when it is on).
//! The plain F1–F12 codes of an Fn-locked row travel the ordinary HID keyboard
//! interface and never reach `0x1b04`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use openlogi_core::binding::ButtonId;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::gesture::{CaptureChannel, CapturedInput, GestureError, enumerate_controls, restore};
use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::write::SharedChannel;

/// The divertable keyboard F-row controls OpenLogi models, as
/// `(0x1b04 control ID, ButtonId)` pairs. CID values match Logitech's control
/// catalog (cross-checked against Solaar's `special_keys.py`); the F-row
/// positions are the Signature-series layout.
pub const KEYBOARD_KEY_CIDS: [(u16, ButtonId); 9] = [
    (0x00d4, ButtonId::KeySearch),
    (0x0103, ButtonId::KeyDictation),
    (0x0108, ButtonId::KeyEmoji),
    (0x010a, ButtonId::KeyScreenCapture),
    (0x011c, ButtonId::KeyMicMute),
    (0x00e5, ButtonId::KeyPlayPause),
    (0x00e7, ButtonId::KeyMute),
    (0x00e8, ButtonId::KeyVolumeDown),
    (0x00e9, ButtonId::KeyVolumeUp),
];

/// Capture the requested keyboard controls on `route` until `shutdown`
/// resolves, forwarding a [`CapturedInput::ButtonPressed`] on each press
/// (rising edge) to `sink`.
///
/// `wanted` maps `0x1b04` control IDs to the [`ButtonId`] they dispatch as —
/// the caller passes only the keys that carry a real binding. Controls the
/// device doesn't expose (or can't divert) are skipped with a debug log, so a
/// partially-supported keyboard degrades per key rather than failing whole.
pub async fn run_keyboard_capture_session(
    route: DeviceRoute,
    wanted: BTreeMap<u16, ButtonId>,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let chan = open_route_channel(&route)
        .await?
        .ok_or(GestureError::DeviceNotFound)?;
    let device_index = route.device_index();
    let device = Device::new(Arc::clone(&chan), device_index)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(device_index))?;

    let info = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
        .ok_or_else(|| GestureError::Hidpp("keyboard exposes no 0x1b04 reprog controls".into()))?;
    let rc = ReprogControlsV4::new(Arc::clone(&chan), device_index, info.index);
    let controls = enumerate_controls(&rc).await?;

    let diverted = arm_keys(&rc, &controls, &wanted).await?;

    // Rising-edge press state per CID. Behind a `Mutex` because the channel's
    // read thread invokes the listener by shared reference.
    let held: Arc<Mutex<BTreeSet<u16>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let feature_index = info.index;
    let listener = chan.add_msg_listener_guarded({
        let held = Arc::clone(&held);
        let diverted = diverted.clone();
        let sink = sink.clone();
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            let Some(RawControlEvent::DivertedButtons(cids)) =
                reprog_controls::decode_event(&msg, device_index, feature_index)
            else {
                return;
            };
            // Recover the guard even if a prior holder panicked — the critical
            // section is panic-free, so the data is consistent.
            let mut down = held.lock().unwrap_or_else(PoisonError::into_inner);
            for (&cid, &button) in &diverted {
                let now = cids.contains(&cid);
                let was = down.contains(&cid);
                if now && !was {
                    let _ = sink.send(CapturedInput::ButtonPressed(button));
                }
                if now {
                    down.insert(cid);
                } else {
                    down.remove(&cid);
                }
            }
        }
    });

    // Wireless keyboards drop their diverted-control state when they
    // power-cycle (idle sleep, power switch, Easy-Switch host change) — the
    // reconnection broadcast on `0x1d4b` is the firmware asking the host to
    // reconfigure. Re-arm the diversion on every broadcast, or the bound keys
    // silently revert to their native functions after the first nap.
    let wireless = device
        .root()
        .get_feature(WirelessDeviceStatusFeature::ID)
        .await
        .ok()
        .flatten()
        .map(|info| WirelessDeviceStatusFeature::new(Arc::clone(&chan), device_index, info.index));
    let wake_events = wireless.as_ref().map(EmittingFeature::listen);

    // Publish this keyboard's open channel so hardware writes (Fn-lock)
    // reuse it instead of opening the same HID node a second time. Cleared
    // on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(SharedChannel::new(Arc::clone(&chan), route.clone()));
    }

    info!(
        index = device_index,
        keys = diverted.len(),
        wake_rearm = wake_events.is_some(),
        "keyboard key capture active"
    );
    let mut shutdown = shutdown;
    match wake_events {
        None => {
            let _ = shutdown.await;
        }
        Some(wake_events) => loop {
            tokio::select! {
                _ = &mut shutdown => break,
                event = wake_events.recv() => {
                    let Ok(WirelessDeviceStatusEvent::StatusBroadcast(broadcast)) = event else {
                        // Emitter gone (feature dropped) — nothing left to
                        // watch; fall back to a plain shutdown wait.
                        let _ = shutdown.await;
                        break;
                    };
                    info!(?broadcast, "keyboard reconnected — re-arming key diversion");
                    rearm_keys(&rc, &diverted).await;
                }
            }
        },
    }

    drop(listener);
    if let Ok(mut slot) = channel_slot.write() {
        *slot = None;
    }
    for &cid in diverted.keys() {
        restore(
            rc.set_cid_reporting(cid, false, false).await,
            "keyboard key",
        );
    }
    debug!(index = device_index, "keyboard key capture stopped");
    Ok(())
}

/// Divert every wanted control the keyboard exposes as divertable, returning
/// the armed `CID → ButtonId` subset. Missing / non-divertable controls are
/// skipped with a debug log, so a partially-supported keyboard degrades per
/// key rather than failing whole.
async fn arm_keys(
    rc: &ReprogControlsV4,
    controls: &[reprog_controls::CtrlIdInfo],
    wanted: &BTreeMap<u16, ButtonId>,
) -> Result<BTreeMap<u16, ButtonId>, GestureError> {
    let mut diverted = BTreeMap::new();
    for (&cid, &button) in wanted {
        if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
            rc.set_cid_reporting(cid, true, false)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            diverted.insert(cid, button);
        } else {
            debug!(
                cid = format_args!("{cid:#06x}"),
                "bound key not divertable on this keyboard — left native"
            );
        }
    }
    Ok(diverted)
}

/// Re-issue diversion for every armed control after a device power-cycle.
/// Failures are logged, not propagated — the next reconnection broadcast
/// retries.
async fn rearm_keys(rc: &ReprogControlsV4, diverted: &BTreeMap<u16, ButtonId>) {
    // A settling pause: the broadcast arrives the instant the link is back,
    // occasionally before the device accepts feature writes again.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    for &cid in diverted.keys() {
        if let Err(e) = rc.set_cid_reporting(cid, true, false).await {
            warn!(
                cid = format_args!("{cid:#06x}"),
                error = ?e,
                "re-divert after wake failed — key stays native until next wake"
            );
        }
    }
}

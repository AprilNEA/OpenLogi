//! Live control capture for one device: divert the MX dedicated gesture button,
//! the DPI/ModeShift button, and the thumb wheel over HID++ — and arm the MX
//! Master 4 Action Ring pad for `analyticsKeyEvents` reporting — turning their
//! events into [`CapturedInput`] the GUI can dispatch.
//!
//! [`run_capture_session`] holds a single HID++ channel open for one device,
//! enables capture on whichever of those controls it exposes, registers one
//! message listener, and restores every control's default reporting on
//! shutdown. Using one channel matters: a second channel to the same device
//! would split its input-report stream, so all captured controls share this
//! session.
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! The GUI maps each [`CapturedInput`] to the user's bound action and dispatches
//! it, mirroring how the CGEventTap hook handles the side buttons. The thumb
//! wheel is special: diverting it stops native horizontal scroll, so the GUI
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when its click is actually bound.

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        force_sensing_button::ForceSensingButtonFeature, reprog_controls::CidReportingChange,
    },
    protocol::v20,
};
use openlogi_core::binding::{ButtonId, GestureDirection, SwipeAccumulator};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::thumbwheel::{self, Thumbwheel};
use crate::write::{SharedChannel, open_feature};

/// Force-activation threshold Options+ writes to the Action Ring pad's button
/// `0` at startup — the magic constant that wakes the physically dormant pad
/// (`docs/mx-master-4-panel-captures/optionsplus-coldstart-full.txt`).
const ACTION_RING_FORCE_THRESHOLD: u16 = 0x15a3;

/// Cadence for re-applying the Action Ring arming recipe. A single arm at
/// session start is lost when the BTLE link is asleep, and the configuration
/// does not survive the device sleeping mid-session, so the recipe is
/// re-applied continuously (the cadence the confirming hardware run used).
const PANEL_REARM_PERIOD: Duration = Duration::from_secs(3);

/// Budget for one Action Ring arming pass. Channel reads have no intrinsic
/// timeout, so without a bound a cold link could hold an arming call — and
/// with it the session's shutdown path — indefinitely.
const PANEL_ARM_BUDGET: Duration = Duration::from_secs(2);

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
    /// Whether any Action Ring CID was down in the last analytics event — for
    /// rising-edge press detection across the pad's multiple CIDs.
    panel_down: bool,
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
        action_ring = armed.panel.is_some(),
        dpi_buttons = armed.dpi_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        "control capture active"
    );

    // The Action Ring's arming does not survive the device sleeping, so a
    // session with the pad re-applies the recipe on a cadence while waiting
    // for shutdown. The interval's first tick fires immediately and doubles as
    // the initial arm — deliberately after the listener above, so no event
    // between arm and listen is lost.
    match (&armed.panel, &armed.reprog) {
        (Some(panel), Some((rc, _))) => {
            let mut shutdown = shutdown;
            let mut cadence = tokio::time::interval(PANEL_REARM_PERIOD);
            cadence.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    _ = cadence.tick() => {
                        if tokio::time::timeout(PANEL_ARM_BUDGET, panel.apply(rc))
                            .await
                            .is_err()
                        {
                            debug!("action ring re-arm timed out (link cold?)");
                        }
                    }
                }
            }
        }
        _ => {
            let _ = shutdown.await;
        }
    }

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
    /// Present when the device has the Action Ring pad — the arming recipe
    /// [`run_capture_session`] re-applies on a cadence.
    panel: Option<PanelArming>,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// `0x2150` accessor + feature index, present when the thumb wheel is
    /// diverted.
    thumb: Option<(Thumbwheel, u8)>,
}

impl ArmedControls {
    /// Restore every captured control. Failures are logged, not propagated.
    async fn disarm(&self) {
        if let Some((rc, _)) = self.reprog.as_ref() {
            if self.gesture_diverted {
                let r = rc
                    .set_cid_reporting(reprog_controls::GESTURE_BUTTON_CID, false, false)
                    .await;
                restore(r, "gesture button");
            }
            if self.panel.is_some() {
                let off = CidReportingChange {
                    analytics_key_events: Some(false),
                    ..CidReportingChange::default()
                };
                for cid in reprog_controls::ACTION_RING_ANALYTICS_CIDS {
                    let r = rc.set_cid_reporting_full(cid, off).await.map(|_| ());
                    restore(r, "action ring pad");
                }
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

/// The Action Ring pad's arming recipe, mirroring what Options+ does at
/// startup: write the pad's force threshold (the pad is physically dormant
/// without one), then enable `analyticsKeyEvents` reporting on its CIDs — no
/// diversion, no raw-XY. Re-applied on [`PANEL_REARM_PERIOD`] because neither
/// write survives the device sleeping.
struct PanelArming {
    /// `0x19c0` accessor for the force-threshold write; `None` when the device
    /// does not expose it (analytics reporting is still armed).
    fsb: Option<Arc<ForceSensingButtonFeature>>,
}

impl PanelArming {
    /// One arming pass. Failures are logged at debug and retried on the next
    /// cadence tick — transient misses are expected while the BTLE link is
    /// asleep.
    async fn apply(&self, rc: &ReprogControlsV4) {
        if let Some(fsb) = &self.fsb
            && let Err(e) = fsb
                .set_force_threshold(0, ACTION_RING_FORCE_THRESHOLD)
                .await
        {
            debug!(error = ?e, "action ring force-threshold write failed");
        }
        let on = CidReportingChange {
            analytics_key_events: Some(true),
            ..CidReportingChange::default()
        };
        for cid in reprog_controls::ACTION_RING_ANALYTICS_CIDS {
            if let Err(e) = rc.set_cid_reporting_full(cid, on).await {
                debug!(cid, error = ?e, "action ring analytics arm failed");
            }
        }
    }
}

/// Resolve features off the device's root and capture the controls we handle:
/// divert the gesture button (raw-XY) and DPI/ModeShift buttons over `0x1b04`,
/// resolve the Action Ring pad's arming recipe when the control table
/// advertises the pad, and — when `capture_thumbwheel` and the wheel reports a
/// single tap — divert the thumb wheel over `0x2150`. The root-feature lookup
/// mirrors `write::open_feature`, since hidpp 0.2's registry doesn't carry the
/// features OpenLogi reimplements.
async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    capture_thumbwheel: bool,
    divert_gesture_button: bool,
) -> Result<ArmedControls, GestureError> {
    let mut device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;

    let mut reprog: Option<(ReprogControlsV4, u8)> = None;
    let mut gesture_diverted = false;
    let mut panel: Option<PanelArming> = None;
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

        // The Action Ring pad advertises `analytics-events` in the control
        // table — never divert it (and never touch 0x00d7; see
        // [`reprog_controls::HAPTIC_PANEL_CID`]). Only resolve the recipe
        // here: the actual arming runs on the session's cadence, after the
        // message listener is registered.
        if controls
            .iter()
            .any(|c| c.cid == reprog_controls::ACTION_RING_CID && c.supports_analytics_events())
        {
            // 0x19c0 wakes the physical pad. Its absence is tolerated in case
            // another device family reports analytics-capable controls
            // without a force pad — analytics reporting is still armed.
            let fsb = match open_feature::<ForceSensingButtonFeature>(&mut device).await {
                Ok(fsb) => Some(fsb),
                Err(e) => {
                    warn!(error = %e, "action ring: 0x19c0 unavailable, arming analytics only");
                    None
                }
            };
            panel = Some(PanelArming { fsb });
            info!("action ring pad present — analytics capture arming on cadence");
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

    if !gesture_diverted && panel.is_none() && dpi_cids.is_empty() && thumb.is_none() {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(ArmedControls {
        reprog,
        gesture_diverted,
        panel,
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
/// release, emit a [`ButtonId::DpiToggle`] press on the rising edge of any
/// diverted DPI/ModeShift control, and track Action Ring taps from analytics
/// entries.
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
        RawControlEvent::AnalyticsKeys(entries) => {
            // The Action Ring pad reports each tap as a press/release pair on
            // ONE of its CIDs (0x01a0 or 0x0050, varying with the press), and
            // a firm press can hold several CIDs at once — so track "any ring
            // CID down" and emit on its rising edge: one physical tap, one
            // press. The release is deliberately unused: the ring UI is a
            // toggle (tap to open, tap to select), not press-and-hold.
            let mut pressed = false;
            let mut released = false;
            for entry in entries {
                let cid: u16 = entry.cid.into();
                if cid == 0 {
                    continue;
                }
                if reprog_controls::ACTION_RING_ANALYTICS_CIDS.contains(&cid) {
                    if entry.event == 0 {
                        released = true;
                    } else {
                        pressed = true;
                    }
                } else {
                    debug!(
                        cid,
                        entry.event, "analytics event from an unhandled control"
                    );
                }
            }
            if pressed && !acc.panel_down {
                acc.panel_down = true;
                // Log-only until the Action Ring is a bindable control: turning
                // this into a CapturedInput needs a ButtonId variant, which
                // crosses the IPC wire (append-only, protocol version bump).
                debug!("action ring pressed");
            } else if released && !pressed {
                acc.panel_down = false;
            }
        }
    }
}
#[cfg(test)]
mod tests;

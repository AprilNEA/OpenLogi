//! Live key capture for one keyboard: divert bound F-row controls over HID++
//! `0x1b04`, enable bound gaming keys over `0x8010`, and turn their physical
//! edges into [`CapturedInput`] the agent can dispatch.
//!
//! [`run_keyboard_capture_session`] is the keyboard counterpart of
//! [`crate::session::gesture::run_capture_session`]: one open channel, diversion armed
//! on exactly the controls the caller asks for (an unbound key is never
//! diverted, so it keeps its native firmware function), listeners for both
//! event families, and every captured control handed back to the firmware on
//! shutdown.
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
        gaming_g_keys::{GKeyState, GamingGKeysEvent, GamingGKeysFeature},
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use openlogi_core::binding::ButtonId;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::capture_restore::{
    ArmedReporting, CaptureStop, ReprogRestore, divert_change, drop_listener_after,
    restore_after_stop, rollback_capture_start, stop_for_current_publication,
    wait_for_channel_change,
};
use super::gesture::{
    CaptureChannel, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput, GestureError,
    PendingCaptureRestore, enumerate_controls,
};
use crate::backend::{BackendError, HidBackend};
use crate::channel::route::{DeviceRoute, open_route_channel};
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};

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

/// G913 gaming keys OpenLogi models, in physical top-to-bottom order.
pub const GAMING_G_KEYS: [(GKeyState, ButtonId); 5] = [
    (GKeyState::G1, ButtonId::KeyG1),
    (GKeyState::G2, ButtonId::KeyG2),
    (GKeyState::G3, ButtonId::KeyG3),
    (GKeyState::G4, ButtonId::KeyG4),
    (GKeyState::G5, ButtonId::KeyG5),
];

/// Bound keyboard controls that one capture session should own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardCaptureTargets {
    /// Divertable `0x1b04` control ID → logical button.
    pub reprog: BTreeMap<u16, ButtonId>,
    /// `0x8010` gaming keys that should emit software events.
    pub gaming: BTreeSet<ButtonId>,
}

/// Capture the requested keyboard controls on `route` until `shutdown`
/// resolves, forwarding [`CapturedInput::ButtonDown`] and
/// [`CapturedInput::ButtonUp`] edges to `sink`.
///
/// `targets.reprog` maps `0x1b04` control IDs to the [`ButtonId`] they dispatch as —
/// the caller passes only the keys that carry a real binding. Controls the
/// device doesn't expose (or can't divert) are skipped with a debug log, so a
/// partially-supported keyboard degrades per key rather than failing whole.
/// `targets.gaming` applies the same bound-only rule to `0x8010` G-key events.
pub async fn run_keyboard_capture_session(
    backend: &dyn HidBackend,
    route: DeviceRoute,
    targets: KeyboardCaptureTargets,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    if !device_io.allows_io() {
        return Err(device_io_suspended().into());
    }
    let chan = open_route_channel(backend, &route)
        .await
        .map_err(GestureError::from)?
        .ok_or(GestureError::DeviceNotFound)?;
    let shared = SharedChannel::new(chan, route.clone());
    run_keyboard_capture_session_on(
        shared,
        targets,
        sink,
        shutdown,
        channel_slot,
        None,
        device_io,
    )
    .await
}

/// Run keyboard capture on the exact channel currently published by `registry`.
///
/// A registry miss returns [`GestureError::DeviceNotFound`] without falling
/// back to route enumeration/opening; the agent watcher retries after a later
/// inventory publication.
pub async fn run_keyboard_capture_session_with_registry(
    route: DeviceRoute,
    targets: KeyboardCaptureTargets,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    registry: &ChannelRegistry,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = registry
        .lookup(&route)
        .ok_or(GestureError::DeviceNotFound)?;
    run_keyboard_capture_session_on(
        shared,
        targets,
        sink,
        shutdown,
        channel_slot,
        Some(registry),
        device_io,
    )
    .await
}

async fn run_keyboard_capture_session_on(
    shared: SharedChannel,
    targets: KeyboardCaptureTargets,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    registry: Option<&ChannelRegistry>,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    if !device_io.allows_io() {
        return Err(device_io_suspended().into());
    }
    let chan = Arc::clone(shared.channel());
    let device_index = shared.device_index();
    let device = Device::new(Arc::clone(&chan), device_index)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(device_index))?;

    let mut armed = ArmedKeys {
        controls: None,
        reporting: Vec::new(),
        diverted: BTreeMap::new(),
        gaming: None,
        gaming_index: None,
        wanted_g_keys: targets.gaming.clone(),
    };

    let setup = arm_capture_targets(&device, &shared, &targets, &mut armed).await;
    if let Err(error) = setup {
        let pending = armed.into_pending(&shared);
        return Err(rollback_capture_start(error, pending, &shared, registry).await);
    }

    // Physical press state per CID. Behind a `Mutex` because the channel's
    // read thread invokes the listener by shared reference.
    let held: Arc<Mutex<BTreeSet<u16>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let feature_index = armed
        .controls
        .as_ref()
        .map_or(u8::MAX, ReprogControlsV4::feature_index);
    let listener = chan.add_msg_listener_guarded({
        let held = Arc::clone(&held);
        let diverted = armed.diverted.clone();
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
            emit_button_edges(&mut down, &cids, &diverted, &sink);
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

    // Publish this keyboard's open channel so hardware writes (Fn-lock)
    // reuse it instead of opening the same HID node a second time. Cleared
    // on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(shared.clone());
    }

    info!(
        index = device_index,
        reprog_keys = armed.diverted.len(),
        g_keys = armed.wanted_g_keys.len(),
        wake_rearm = wireless.is_some(),
        "keyboard key capture active"
    );
    let stop = monitor_keyboard_capture(
        KeyboardMonitor {
            armed: &armed,
            device_index,
            registry,
            shared: &shared,
            sink: &sink,
        },
        wireless,
        shutdown,
        device_io,
    )
    .await;

    // The slot is a last-writer-wins cell, so a sibling session may have
    // published its own channel after ours. Clear it only while it still
    // holds *this* session's channel — evicting the sibling's would silently
    // demote its hardware writes to the fresh-open slow path (the gesture
    // session applies the same discipline).
    if let Ok(mut slot) = channel_slot.write()
        && slot
            .as_ref()
            .is_some_and(|shared| Arc::ptr_eq(shared.channel(), &chan))
    {
        *slot = None;
    }
    let (pending, gaming_listener) = armed.into_restore(&shared);
    // Keep accepting edges until firmware restoration is complete. The agent
    // drains this listener's forwarding task before publishing ordered Done,
    // so this session remains the sole owner of every input captured while
    // its controls could still be diverted.
    let outcome = drop_listener_after(
        (listener, gaming_listener),
        restore_after_stop(stop, pending, &shared, registry),
    )
    .await;
    debug!(index = device_index, "keyboard key capture stopped");
    Ok(outcome)
}

struct KeyboardMonitor<'a> {
    armed: &'a ArmedKeys,
    device_index: u8,
    registry: Option<&'a ChannelRegistry>,
    shared: &'a SharedChannel,
    sink: &'a mpsc::UnboundedSender<CapturedInput>,
}

async fn arm_capture_targets(
    device: &Device,
    shared: &SharedChannel,
    targets: &KeyboardCaptureTargets,
    armed: &mut ArmedKeys,
) -> Result<(), GestureError> {
    let chan = shared.channel();
    let device_index = shared.device_index();
    if !targets.reprog.is_empty() {
        let info = device
            .root()
            .get_feature(reprog_controls::FEATURE_ID)
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?
            .ok_or_else(|| {
                GestureError::Hidpp("keyboard exposes no 0x1b04 reprog controls".into())
            })?;
        let controls = ReprogControlsV4::new(Arc::clone(chan), device_index, info.index);
        let available = enumerate_controls(&controls).await?;
        armed.controls = Some(controls);
        arm_keys(&available, &targets.reprog, armed).await?;
    }

    if targets.gaming.is_empty() {
        return Ok(());
    }
    let info = device
        .root()
        .get_feature(GamingGKeysFeature::ID)
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?
        .ok_or_else(|| GestureError::Hidpp("keyboard exposes no 0x8010 GamingGKeys".into()))?;
    let gaming = GamingGKeysFeature::new(Arc::clone(chan), device_index, info.index);
    // Record ownership before the write: a transport failure does not prove
    // that firmware ignored the mode transition.
    armed.gaming_index = Some(info.index);
    gaming
        .set_software_control(true)
        .await
        .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
    armed.gaming = Some(gaming);
    Ok(())
}

async fn monitor_keyboard_capture(
    context: KeyboardMonitor<'_>,
    wireless: Option<WirelessDeviceStatusFeature>,
    shutdown: oneshot::Receiver<()>,
    mut device_io: DeviceIoGate,
) -> CaptureStop {
    let mut wake_events = wireless.as_ref().map(EmittingFeature::listen);
    let mut g_key_events = context.armed.gaming.as_ref().map(EmittingFeature::listen);
    let mut g_keys_down = GKeyState::empty();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        if !device_io.allows_io() && !device_io.wait_until_allowed().await {
            return stop_for_current_publication(context.registry, context.shared);
        }
        tokio::select! {
            biased;

            allowed = device_io.changed() => {
                if allowed.is_none() {
                    return stop_for_current_publication(context.registry, context.shared);
                }
            }
            _ = &mut shutdown => {
                return stop_for_current_publication(context.registry, context.shared);
            }
            transition = wait_for_channel_change(context.registry, context.shared) => {
                info!(index = context.device_index, "inventory replaced or removed keyboard capture channel — restarting session");
                return transition;
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
                info!(?broadcast, "keyboard reconnected — re-arming key diversion");
                rearm_keys(context.armed, &device_io).await;
            }
            event = async {
                match g_key_events.as_ref() {
                    Some(events) => events.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let Some(GamingGKeysEvent::StateChanged(state)) = event else {
                    g_key_events = None;
                    continue;
                };
                emit_g_key_edges(
                    &mut g_keys_down,
                    state,
                    &context.armed.wanted_g_keys,
                    context.sink,
                );
            }
        }
    }
}

fn emit_g_key_edges(
    down: &mut GKeyState,
    state: GKeyState,
    wanted: &BTreeSet<ButtonId>,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    for (flag, button) in GAMING_G_KEYS {
        if !wanted.contains(&button) {
            continue;
        }
        let now = state.contains(flag);
        let was = down.contains(flag);
        if now && !was {
            let _ = sink.send(CapturedInput::ButtonDown(button));
        } else if !now && was {
            let _ = sink.send(CapturedInput::ButtonUp(button));
        }
    }
    *down = state;
}

/// Diff one full diverted-control snapshot into exactly one edge per physical
/// transition. Unchanged snapshots are deliberately silent.
fn emit_button_edges(
    down: &mut BTreeSet<u16>,
    cids: &[u16],
    diverted: &BTreeMap<u16, ButtonId>,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    for (&cid, &button) in diverted {
        let now = cids.contains(&cid);
        let was = down.contains(&cid);
        if now && !was {
            let _ = sink.send(CapturedInput::ButtonDown(button));
        } else if !now && was {
            let _ = sink.send(CapturedInput::ButtonUp(button));
        }
        if now {
            down.insert(cid);
        } else {
            down.remove(&cid);
        }
    }
}

struct ArmedKeys {
    controls: Option<ReprogControlsV4>,
    reporting: Vec<ArmedReporting>,
    diverted: BTreeMap<u16, ButtonId>,
    gaming: Option<GamingGKeysFeature>,
    gaming_index: Option<u8>,
    wanted_g_keys: BTreeSet<ButtonId>,
}

impl ArmedKeys {
    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        self.into_restore(retired).0
    }

    fn into_restore(
        mut self,
        retired: &SharedChannel,
    ) -> (Option<PendingCaptureRestore>, Option<GamingGKeysFeature>) {
        let gaming_listener = self.gaming.take();
        let reprog = self
            .controls
            .as_ref()
            .and_then(|controls| ReprogRestore::new(controls.feature_index(), self.reporting));
        (
            PendingCaptureRestore::new(retired, reprog, None, self.gaming_index),
            gaming_listener,
        )
    }
}

/// Divert every wanted control the keyboard exposes, adding successful CIDs to
/// dispatch state and every possibly-applied write to rollback state. Missing
/// or non-divertable controls are skipped so support degrades per key.
async fn arm_keys(
    controls: &[reprog_controls::CtrlIdInfo],
    wanted: &BTreeMap<u16, ButtonId>,
    armed: &mut ArmedKeys,
) -> Result<(), GestureError> {
    for (&cid, &button) in wanted {
        if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
            let Some(controls) = armed.controls.as_ref() else {
                return Err(GestureError::Hidpp(
                    "keyboard reprog controls disappeared during setup".into(),
                ));
            };
            let original = controls
                .get_cid_reporting(cid)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
            // A transport failure does not prove the firmware rejected the
            // command, so include this CID in rollback before writing.
            armed.reporting.push(ArmedReporting { cid, original });
            controls
                .set_cid_reporting_full(cid, divert_change(original, false))
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            armed.diverted.insert(cid, button);
        } else {
            debug!(
                cid = format_args!("{cid:#06x}"),
                "bound key not divertable on this keyboard — left native"
            );
        }
    }
    Ok(())
}

/// Re-issue diversion for every armed control after a device power-cycle.
/// Failures are logged, not propagated — the next reconnection broadcast
/// retries.
async fn rearm_keys(armed: &ArmedKeys, device_io: &DeviceIoGate) {
    // A settling pause: the broadcast arrives the instant the link is back,
    // occasionally before the device accepts feature writes again.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    if !device_io.allows_io() {
        return;
    }
    for &reporting in &armed.reporting {
        let Some(controls) = armed.controls.as_ref() else {
            break;
        };
        if let Err(e) = controls
            .set_cid_reporting_full(reporting.cid, divert_change(reporting.original, false))
            .await
        {
            warn!(
                cid = format_args!("{:#06x}", reporting.cid),
                error = ?e,
                "re-divert after wake failed — key stays native until next wake"
            );
        }
    }
    if let Some(gaming) = &armed.gaming
        && let Err(error) = gaming.set_software_control(true).await
    {
        warn!(%error, "re-enable G-key software control after wake failed");
    }
}

fn device_io_suspended() -> GestureError {
    GestureError::Hid(BackendError::Backend("host device I/O is suspended".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_snapshots_emit_balanced_edges_without_duplicates() {
        let diverted = BTreeMap::from([
            (0x00d4, ButtonId::KeySearch),
            (0x0103, ButtonId::KeyDictation),
        ]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        emit_button_edges(&mut down, &[0x00d4], &diverted, &sink);
        emit_button_edges(&mut down, &[0x00d4], &diverted, &sink);
        emit_button_edges(&mut down, &[0x00d4, 0x0103], &diverted, &sink);
        emit_button_edges(&mut down, &[0x0103], &diverted, &sink);
        emit_button_edges(&mut down, &[], &diverted, &sink);

        assert_eq!(
            std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                CapturedInput::ButtonDown(ButtonId::KeySearch),
                CapturedInput::ButtonDown(ButtonId::KeyDictation),
                CapturedInput::ButtonUp(ButtonId::KeySearch),
                CapturedInput::ButtonUp(ButtonId::KeyDictation),
            ]
        );
    }

    #[test]
    fn gaming_key_snapshots_emit_only_wanted_balanced_edges() {
        let wanted = BTreeSet::from([ButtonId::KeyG1, ButtonId::KeyG5]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = GKeyState::empty();

        emit_g_key_edges(&mut down, GKeyState::G1, &wanted, &sink);
        emit_g_key_edges(
            &mut down,
            GKeyState::G1 | GKeyState::G3 | GKeyState::G5,
            &wanted,
            &sink,
        );
        emit_g_key_edges(&mut down, GKeyState::G5, &wanted, &sink);
        emit_g_key_edges(&mut down, GKeyState::empty(), &wanted, &sink);

        assert_eq!(
            std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                CapturedInput::ButtonDown(ButtonId::KeyG1),
                CapturedInput::ButtonDown(ButtonId::KeyG5),
                CapturedInput::ButtonUp(ButtonId::KeyG1),
                CapturedInput::ButtonUp(ButtonId::KeyG5),
            ]
        );
    }
}

//! Live key capture for one keyboard: divert the bound `0x1b04` controls and
//! turn their physical edges into [`CapturedInput`] the agent can dispatch.
//!
//! [`run_keyboard_capture_session`] is the keyboard counterpart of
//! [`crate::session::gesture::run_capture_session`], and runs the same channel
//! lifecycle: one open channel, one message listener, and every diverted
//! control handed back to the firmware on shutdown. What is its own is the
//! arming — diversion on exactly the controls the caller asks for (an unbound
//! key is never diverted, so it keeps its native firmware function) — and the
//! edge decoding.
//!
//! Which controls exist is the keyboard's business: the caller names them by
//! control ID ([`ButtonId::Control`]) and this session diverts whichever of
//! them the device's own `0x1b04` table reports as divertable, so a key
//! OpenLogi has no catalog row for is captured exactly like a known one.
//!
//! Diversion works on the key's *control* — the printed media/shortcut
//! function — so it fires when Fn-lock is off (or via Fn+key when it is on).
//! The plain F1–F12 codes of an Fn-locked row travel the ordinary HID keyboard
//! interface and never reach `0x1b04`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::protocol::v20;
use openlogi_core::binding::ButtonId;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::capture::{ArmedCapture, CaptureHost, Liveness, open_device, run_capture};
use super::capture_restore::{ArmedReporting, ReprogRestore, divert_change};
use super::gesture::{
    CaptureError, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput,
    PendingCaptureRestore, enumerate_controls,
};
use super::restore::rollback_start;
use crate::channel::route::DeviceRoute;
use crate::{ChannelRegistry, SharedChannel};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};

/// Controls that are never diverted as keyboard keys, whatever a binding says.
///
/// - `0x00d1`–`0x00d3`, the Easy-Switch host keys: the host-switch session
///   owns their diversion on the same channel, and a second owner would read
///   its diverted state back as "original" and dispatch every press twice.
/// - `0x0050`/`0x0051`, the primary clicks of a keyboard with a built-in
///   touchpad: swallowing them would leave the user without a pointer, the
///   same reason the mouse hook never suppresses them.
///
/// The config layer still accepts these names — a file is not invalid for
/// naming a control — so the policy lives here, next to the hardware write,
/// and is applied again by the orchestrator so a binding that names only
/// reserved controls publishes no session at all.
pub const RESERVED_KEYBOARD_CONTROLS: [u16; 5] = [
    reprog_controls::control_ids::HOST_SWITCH_CHANNEL_1.0,
    reprog_controls::control_ids::HOST_SWITCH_CHANNEL_2.0,
    reprog_controls::control_ids::HOST_SWITCH_CHANNEL_3.0,
    0x0050,
    0x0051,
];

/// Whether `cid` is one of the [`RESERVED_KEYBOARD_CONTROLS`].
#[must_use]
pub fn is_reserved_keyboard_control(cid: u16) -> bool {
    RESERVED_KEYBOARD_CONTROLS.contains(&cid)
}

/// Capture the requested keyboard controls on `route` until `host.shutdown`
/// resolves, forwarding [`CapturedInput::ButtonDown`] and
/// [`CapturedInput::ButtonUp`] edges to `host.sink`.
///
/// `wanted` maps `0x1b04` control IDs to the [`ButtonId`] they dispatch as —
/// the caller passes only the keys that carry a real binding. Controls the
/// device doesn't expose (or can't divert) are skipped with a debug log, so a
/// partially-supported keyboard degrades per key rather than failing whole.
///
/// Runs on the exact channel currently published by `host.registry`. A
/// registry miss returns [`CaptureError::DeviceNotFound`] without falling back
/// to route enumeration/opening; the agent watcher retries after a later
/// inventory publication.
pub async fn run_keyboard_capture_session(
    route: DeviceRoute,
    wanted: BTreeMap<u16, ButtonId>,
    host: CaptureHost<'_>,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = host.channel_for(&route)?;
    let armed = arm_keyboard(&shared, &wanted, host.registry).await?;
    Ok(run_capture(shared, armed, host).await)
}

/// Divert the wanted controls of the keyboard behind `shared`.
///
/// A failure mid-way tries to hand every possibly-diverted control back to the
/// firmware. If compensation is incomplete, the returned failure carries an
/// opaque restore capability for the manager to retain and retry.
async fn arm_keyboard(
    shared: &SharedChannel,
    wanted: &BTreeMap<u16, ButtonId>,
    registry: &ChannelRegistry,
) -> Result<ArmedKeys, CaptureSessionFailure> {
    let device = open_device(shared).await?;
    let info = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(CaptureError::from)?
        .ok_or_else(|| CaptureError::Hidpp("keyboard exposes no 0x1b04 reprog controls".into()))?;
    let rc = ReprogControlsV4::new(
        Arc::clone(shared.channel()),
        shared.device_index(),
        info.index,
    );
    let controls = enumerate_controls(&rc).await?;
    let mut armed = ArmedKeys {
        controls: rc,
        reporting: Vec::new(),
        diverted: BTreeMap::new(),
    };
    if let Err(error) = arm_keys(&controls, wanted, &mut armed).await {
        let pending = armed.into_pending(shared);
        return Err(rollback_start(error, pending, registry).await);
    }
    Ok(armed)
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
    controls: ReprogControlsV4,
    reporting: Vec<ArmedReporting>,
    diverted: BTreeMap<u16, ButtonId>,
}

impl ArmedCapture for ArmedKeys {
    const NAME: &'static str = "keyboard key";
    const LIVENESS: Liveness = Liveness::Unwatched;

    fn log_active(&self, device_index: u8, wake_rearm: bool) {
        info!(
            index = device_index,
            keys = self.diverted.len(),
            wake_rearm,
            "keyboard key capture active"
        );
    }

    fn report_handler(
        &self,
        device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&v20::Message) + Send + Sync + 'static {
        // Physical press state per CID. Behind a `Mutex` because the channel's
        // read thread invokes the handler by shared reference.
        let held: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());
        let feature_index = self.controls.feature_index();
        let diverted = self.diverted.clone();
        move |msg| {
            let Some(RawControlEvent::DivertedButtons(cids)) =
                reprog_controls::decode_event(msg, device_index, feature_index)
            else {
                return;
            };
            // Recover the guard even if a prior holder panicked — the critical
            // section is panic-free, so the data is consistent.
            let mut down = held.lock().unwrap_or_else(PoisonError::into_inner);
            emit_button_edges(&mut down, &cids, &diverted, &sink);
        }
    }

    async fn rearm(&self) -> bool {
        for &reporting in &self.reporting {
            if let Err(e) = self
                .controls
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
        true
    }

    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        let feature_index = self.controls.feature_index();
        PendingCaptureRestore::new(
            retired,
            ReprogRestore::new(feature_index, self.reporting),
            None,
            None,
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
) -> Result<(), CaptureError> {
    for (&cid, &button) in wanted {
        if is_reserved_keyboard_control(cid) {
            warn!(
                cid = format_args!("{cid:#06x}"),
                "reserved control requested as a keyboard key — left native"
            );
            continue;
        }
        if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
            let original = armed.controls.get_cid_reporting(cid).await?;
            // A transport failure does not prove the firmware rejected the
            // command, so include this CID in rollback before writing.
            armed.reporting.push(ArmedReporting { cid, original });
            armed
                .controls
                .set_cid_reporting_full(cid, divert_change(original, false))
                .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::KNOWN_CONTROLS;

    /// The keyboard catalog names keys; the mouse capture path owns the
    /// standard-button, gesture-source and DPI-shift control families. One
    /// control must not be both, or a device would carry two `ButtonId`s for
    /// one key — so the catalog never claims a family member or a reserved
    /// control (they remain bindable as `control:0x…`).
    #[test]
    fn keyboard_catalog_never_claims_a_mouse_owned_or_reserved_control() {
        let mouse_owned: Vec<u16> = reprog_controls::BACK_CIDS
            .into_iter()
            .chain(reprog_controls::FORWARD_CIDS)
            .chain(reprog_controls::DPI_MODE_SHIFT_CIDS)
            .chain([
                reprog_controls::GESTURE_BUTTON_CID,
                reprog_controls::HAPTIC_PANEL_CID,
            ])
            .chain(RESERVED_KEYBOARD_CONTROLS)
            .collect();
        let claimed: Vec<_> = KNOWN_CONTROLS
            .iter()
            .filter(|control| mouse_owned.contains(&control.id.raw()))
            .map(|control| control.name)
            .collect();
        assert!(claimed.is_empty(), "{claimed:?}");
    }

    #[test]
    fn keyboard_snapshots_emit_balanced_edges_without_duplicates() {
        let diverted = BTreeMap::from([
            (0x00d4, ButtonId::control(0x00d4)),
            (0x0103, ButtonId::control(0x0103)),
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
                CapturedInput::ButtonDown(ButtonId::control(0x00d4)),
                CapturedInput::ButtonDown(ButtonId::control(0x0103)),
                CapturedInput::ButtonUp(ButtonId::control(0x00d4)),
                CapturedInput::ButtonUp(ButtonId::control(0x0103)),
            ]
        );
    }
}

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
//! A keypad's grid keys join the same session through the channel's raw-report
//! listener: the MX Creative Keypad reports them
//! on a vendor report sharing its HID++ node, which never parses as a message,
//! so they cannot ride diversion. They are observed, never armed — the session
//! writes nothing on their behalf, and only the indices named in
//! [`KeyboardWanted::grid`] emit edges.
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

use crate::keypad_vendor::{VendorFunction, input_payload};

use super::capture::{
    ArmedCapture, CaptureHost, Liveness, ReportingChangeStream, open_device, run_capture,
    subscribe_reporting_changes,
};
use super::capture_restore::{ArmedReporting, ReprogRestore, divert_keyboard_key};
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

/// Which keys the keyboard session captures, by the plane each one reports
/// on. Both maps dispatch the edges they emit as the [`ButtonId`] the caller
/// chose.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyboardWanted {
    /// `0x1b04` controls to divert, keyed by control ID. Controls the device
    /// doesn't expose (or can't divert) are skipped with a debug log, so a
    /// partially-supported keyboard degrades per key rather than failing whole.
    pub controls: BTreeMap<u16, ButtonId>,
    /// Keypad grid keys to observe, keyed by the wire index the vendor report
    /// carries (the numbering [`openlogi_core::binding::GridKey`] pins).
    /// Arming them writes nothing to the device.
    pub grid: BTreeMap<u8, ButtonId>,
}

/// Capture the requested keyboard controls on `route` until `host.shutdown`
/// resolves, forwarding [`CapturedInput::ButtonDown`] and
/// [`CapturedInput::ButtonUp`] edges to `host.sink`.
///
/// `wanted.controls` names `0x1b04` controls to divert and `wanted.grid` the
/// keypad grid keys to observe; both map to the [`ButtonId`] their edges
/// dispatch as, and the caller passes only the keys that carry a real binding.
///
/// Runs on the exact channel currently published by `host.registry`. A
/// registry miss returns [`CaptureError::DeviceNotFound`] without falling back
/// to route enumeration/opening; the agent watcher retries after a later
/// inventory publication.
pub async fn run_keyboard_capture_session(
    route: DeviceRoute,
    wanted: KeyboardWanted,
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
    wanted: &KeyboardWanted,
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
    // Subscribed before the first diversion write (see
    // `subscribe_reporting_changes`).
    let reporting_changes = Some(subscribe_reporting_changes(&rc));
    let mut armed = ArmedKeys {
        controls: rc,
        reporting: Vec::new(),
        diverted: BTreeMap::new(),
        reporting_changes,
        grid: wanted.grid.clone(),
    };
    if let Err(error) = arm_keys(&controls, &wanted.controls, &mut armed).await {
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

/// Marks a grid-key frame's payload as the held-id list.
const GRID_ID_LIST: u8 = 0x01;

/// Decode one keypad grid-key vendor PDU into the ids it reports held.
///
/// The MX Creative Keypad's framing, hardware-verified 2026-10: a 32-byte
/// report `13 ff 02 00 [seq] 01 [id, …]` (channel framing owned by
/// [`crate::keypad_vendor`]) whose NUL-terminated payload lists
/// every grid key held *now* — a press names the key, the release that
/// follows names none, and a chord names every held key in one frame. `seq`
/// is a rolling counter (`c0`…`cf` on a live capture) that carries no edge
/// of its own; the reference decoder (`node-logitech-mx-creative-console`)
/// reads the payload the same way. Both payload selectors are pinned (`00`
/// sub-report, `01` id-list marker). Anything else on the node — HID++
/// replies, drawing acks, other vendor traffic — decodes to `None`; the
/// index table itself is owned by [`openlogi_core::binding::GridKey`], so
/// this decodes a frame, not identity.
fn grid_key_frame(bytes: &[u8]) -> Option<impl Iterator<Item = u8> + '_> {
    match input_payload(bytes, VendorFunction::GridKeys)? {
        [_seq, GRID_ID_LIST, ids @ ..] if !ids.is_empty() => {
            Some(ids.iter().copied().take_while(|id| *id != 0))
        }
        _ => None,
    }
}

/// Diff one grid-key vendor frame's held set against `down` into edges: a
/// bound key the frame newly holds goes down, a held one it no longer names
/// comes up, releases first. Repeated frames change nothing, and unbound
/// indices never enter the state, so their firmware function is all they do.
fn emit_grid_edges(
    down: &mut BTreeSet<u8>,
    bytes: &[u8],
    grid: &BTreeMap<u8, ButtonId>,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    let Some(ids) = grid_key_frame(bytes) else {
        return;
    };
    let held: BTreeSet<u8> = ids.filter(|id| grid.contains_key(id)).collect();
    for index in down.difference(&held) {
        let _ = sink.send(CapturedInput::ButtonUp(grid[index]));
    }
    for index in held.difference(down) {
        let _ = sink.send(CapturedInput::ButtonDown(grid[index]));
    }
    *down = held;
}

struct ArmedKeys {
    controls: ReprogControlsV4,
    reporting: Vec<ArmedReporting>,
    diverted: BTreeMap<u16, ButtonId>,
    /// Other clients' reporting changes on `controls`, until the running
    /// session takes them.
    reporting_changes: Option<ReportingChangeStream>,
    /// Bound keypad grid keys, keyed by the wire index. No firmware state is
    /// owned here — these keys report whatever the binding does.
    grid: BTreeMap<u8, ButtonId>,
}

impl ArmedCapture for ArmedKeys {
    const NAME: &'static str = "keyboard key";
    const LIVENESS: Liveness = Liveness::Unwatched;

    fn log_active(&self, device_index: u8, wake_rearm: bool) {
        info!(
            index = device_index,
            keys = self.diverted.len(),
            grid_keys = self.grid.len(),
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

    fn raw_report_handler(
        &self,
        _device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&[u8]) + Send + Sync + 'static {
        // Physical press state per grid index, deduplicating repeated frames
        // so edges stay balanced. Kept in a `Mutex` for the same reason as the
        // control-held set above.
        let held: Mutex<BTreeSet<u8>> = Mutex::new(BTreeSet::new());
        let grid = self.grid.clone();
        move |bytes| {
            let mut down = held.lock().unwrap_or_else(PoisonError::into_inner);
            emit_grid_edges(&mut down, bytes, &grid, &sink);
        }
    }

    async fn rearm(&self) {
        for &reporting in &self.reporting {
            if let Err(e) = self
                .controls
                .set_cid_reporting_full(reporting.cid, divert_keyboard_key(reporting.original))
                .await
            {
                warn!(
                    cid = format_args!("{:#06x}", reporting.cid),
                    error = ?e,
                    "re-divert after wake failed — key stays native until next wake"
                );
            }
        }
    }

    fn armed_reporting(&self) -> Option<(&ReprogControlsV4, &[ArmedReporting])> {
        (!self.reporting.is_empty()).then_some((&self.controls, self.reporting.as_slice()))
    }

    fn take_reporting_changes(&mut self) -> Option<ReportingChangeStream> {
        let changes = self.reporting_changes.take()?;
        (!self.reporting.is_empty()).then_some(changes)
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
            armed
                .reporting
                .push(ArmedReporting::keyboard(cid, original));
            armed
                .controls
                .set_cid_reporting_full(cid, divert_keyboard_key(original))
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
    use openlogi_core::binding::{GridKey, KNOWN_CONTROLS};

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

    /// A grid PDU as the keypad sends it: `13 ff 02 00 [seq] 01 [id, …]`
    /// padded to the 32 bytes the node carries.
    fn grid_pdu(seq: u8, ids: &[u8]) -> Vec<u8> {
        let mut pdu = vec![0x13, 0xff, 0x02, 0x00, seq, 0x01];
        pdu.extend_from_slice(ids);
        pdu.resize(32, 0);
        pdu
    }

    fn frame_ids(bytes: &[u8]) -> Option<Vec<u8>> {
        grid_key_frame(bytes).map(Iterator::collect)
    }

    fn drain(inputs: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
        std::iter::from_fn(|| inputs.try_recv().ok()).collect()
    }

    #[test]
    fn grid_frames_decode_the_held_ids() {
        // Captured on the keypad: key 8 pressed, then released.
        assert_eq!(frame_ids(&grid_pdu(0xc8, &[8])), Some(vec![8]));
        assert_eq!(frame_ids(&grid_pdu(0xc9, &[])), Some(vec![]));
        // An HID++ long report sharing the node, and a truncated frame.
        assert_eq!(frame_ids(&[0x11, 0xff, 0x04, 0x1a, 0x0b, 0xb8, 0, 0]), None);
        assert_eq!(frame_ids(&[0x13, 0xff, 0x02, 0x00, 0x01]), None);
    }

    #[test]
    fn grid_frames_reject_wrong_payload_selectors() {
        // The `13 ff 02` prefix alone must not decode: the LCD image-write
        // ack (`13 ff 02 2b c1 00 01`) and drawing-style acks name what
        // would be a valid key byte.
        let mut ack = vec![0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01];
        ack.resize(32, 0);
        assert_eq!(frame_ids(&ack), None);
        // And a frame with the id-list marker replaced also decodes to nothing.
        assert_eq!(frame_ids(&[0x13, 0xff, 0x02, 0x00, 0x01, 0x02, 0x05]), None);
    }

    /// The live sequence: a press frame names the key and the release frame
    /// names none, whatever the rolling counter's parity — the counter wraps
    /// from `cf` to `c0` mid-capture, and a press lands on even and odd
    /// values alike.
    #[test]
    fn a_press_and_its_empty_release_frame_make_one_edge_pair() {
        let grid = BTreeMap::from([
            (8, ButtonId::grid(GridKey::BottomMiddle)),
            (9, ButtonId::grid(GridKey::BottomRight)),
        ]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        for (seq, ids) in [(0xce, &[8][..]), (0xcf, &[]), (0xc0, &[9]), (0xc1, &[])] {
            emit_grid_edges(&mut down, &grid_pdu(seq, ids), &grid, &sink);
        }

        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::BottomMiddle)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::BottomMiddle)),
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::BottomRight)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::BottomRight)),
            ]
        );
        assert_eq!(down, BTreeSet::default());
    }

    #[test]
    fn grid_edges_are_balanced_and_ignore_unbound_keys() {
        let grid = BTreeMap::from([(2, ButtonId::grid(GridKey::TopMiddle))]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        // A repeated frame must not double the edge, and an unbound key —
        // alone or held with a bound one — must not touch the state.
        emit_grid_edges(&mut down, &grid_pdu(0x01, &[2]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x02, &[2]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x03, &[2, 7]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x04, &[7]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x05, &[]), &grid, &sink);

        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::TopMiddle)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::TopMiddle)),
            ]
        );
        assert_eq!(down, BTreeSet::default());
    }

    #[test]
    fn grid_chords_emit_one_balanced_edge_pair_per_key() {
        let grid = BTreeMap::from([
            (3, ButtonId::grid(GridKey::TopRight)),
            (8, ButtonId::grid(GridKey::BottomMiddle)),
        ]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        // Press both as a chord, release 3 while 8 stays held, then release 8.
        emit_grid_edges(&mut down, &grid_pdu(0x01, &[3, 8]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x02, &[8]), &grid, &sink);
        emit_grid_edges(&mut down, &grid_pdu(0x03, &[]), &grid, &sink);

        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::TopRight)),
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::BottomMiddle)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::TopRight)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::BottomMiddle)),
            ]
        );
        assert_eq!(down, BTreeSet::default());
    }
}

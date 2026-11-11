//! Live key capture for one keyboard: divert the bound `0x1b04` controls and
//! turn their physical edges into [`CapturedInput`] the agent can dispatch.
//!
//! [`run_keyboard_capture_session`] is the keyboard counterpart of
//! [`crate::session::gesture::run_capture_session`], and runs the same channel
//! lifecycle: one open channel, one message listener, and every diverted
//! control handed back to the firmware on shutdown. What is its own is the
//! arming — diversion on exactly the controls the caller asks for (an unbound
//! key is never diverted, so it keeps its native firmware function), kept in
//! step with that request while the session runs — and the edge decoding.
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
//! A G-series keyboard's gaming cluster joins the same session too. Its G-key
//! column (HID++ `0x8010`) has no per-key diversion: the session takes the
//! whole row from its onboard actions only when [`KeyboardWanted::gaming`]
//! asks for it — the user's explicit opt-in, never a saved binding — and
//! hands it back on teardown. While the row is taken, the M1–M3 (`0x8020`) and
//! MR (`0x8030`) keys report too; the session listens to the ones the owner
//! names and writes nothing for them.
//!
//! Diversion works on the key's *control* — the printed media/shortcut
//! function — so it fires when Fn-lock is off (or via Fn+key when it is on).
//! The plain F1–F12 codes of an Fn-locked row travel the ordinary HID keyboard
//! interface and never reach `0x1b04`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hidpp::feature::CreatableFeature as _;
use hidpp::feature::gaming_g_keys::{GKeys, GamingGKeysEvent, GamingGKeysFeature};
use hidpp::feature::gaming_m_keys::{self, GamingMKeysEvent, MKeys};
use hidpp::feature::macro_record::{self, MacroRecordEvent};
use hidpp::feature::multi_roller::{
    MultiRollerFeature, RollerId, RollerReportingMode, RollerRotation,
};
use hidpp::protocol::v20;
use openlogi_core::binding::{ButtonId, GKey, ModeKey, Rotary};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::keypad_vendor::{VendorFunction, input_payload};

use hidpp::channel::HidppChannel;
use hidpp::feature::root::RootFeature;

use super::capture::{
    ArmedCapture, CaptureHost, Liveness, ReportingChangeStream, open_device, run_capture,
    subscribe_reporting_changes,
};
use super::capture_restore::{
    ArmedReporting, GKeysRestore, ReprogRestore, RollerRestore, divert_keyboard_key,
};
use super::gesture::{
    CaptureError, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput,
    PendingCaptureRestore, enumerate_controls,
};
use super::restore::rollback_start;
use crate::channel::route::DeviceRoute;
use crate::{ChannelRegistry, SharedChannel};

use crate::reprog_controls::{self, CtrlIdInfo, RawControlEvent, ReprogControlsV4};

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
    /// The G-series gaming cluster: whether to take the G-key row, and
    /// which keys above it to listen to.
    pub gaming: GamingWanted,
    /// Dialpad rotary controls to divert over `0x4610` MultiRoller. Only a
    /// known dialpad ([`DeviceRoute::dialpad`]) says which roller is which
    /// control; on any other device they are skipped with a debug log.
    ///
    /// [`DeviceRoute::dialpad`]: openlogi_core::hid::DeviceRoute::dialpad
    pub rotary: BTreeSet<Rotary>,
}

/// What a session does with a G-series keyboard's gaming cluster.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GamingWanted {
    /// Take the whole `0x8010` G-key row from its onboard actions. Every
    /// G-key then reports as [`ButtonId::g_key`], bound or not.
    pub take_g_row: bool,
    /// The M1–M3 / MR keys whose presses to report. They report only while
    /// the row is taken.
    pub mode_keys: BTreeSet<ModeKey>,
}

/// Capture the requested keyboard controls on `route` until `host.shutdown`
/// resolves, forwarding [`CapturedInput::ButtonDown`] and
/// [`CapturedInput::ButtonUp`] edges to `host.sink`.
///
/// `wanted` names the keys to capture as a [`KeyboardWanted`]: `0x1b04`
/// controls to divert and keypad grid keys to observe, each mapped to the
/// [`ButtonId`] its edges dispatch as. The caller publishes only the keys
/// that carry a real binding, possibly none. The session follows every new
/// value in place: it hands back the controls that left the set, diverts the
/// controls that joined it, and does not write a control that stays; grid
/// keys need no write at all. A per-app binding change therefore costs a few
/// writes instead of a full re-arm, during which every key would run its
/// native function (#2140).
///
/// Runs on the exact channel currently published by `host.registry`. A
/// registry miss returns [`CaptureError::DeviceNotFound`] without falling back
/// to route enumeration/opening; the agent watcher retries after a later
/// inventory publication.
pub async fn run_keyboard_capture_session(
    route: DeviceRoute,
    wanted: watch::Receiver<KeyboardWanted>,
    host: CaptureHost<'_>,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = host.channel_for(&route)?;
    let armed = arm_keyboard(&shared, wanted, host.registry).await?;
    Ok(run_capture(shared, armed, host).await)
}

/// Divert the wanted controls of the keyboard behind `shared`.
///
/// A failure mid-way tries to hand every possibly-diverted control back to the
/// firmware. If compensation is incomplete, the returned failure carries an
/// opaque restore capability for the manager to retain and retry.
async fn arm_keyboard(
    shared: &SharedChannel,
    wanted: watch::Receiver<KeyboardWanted>,
    registry: &ChannelRegistry,
) -> Result<ArmedKeys, CaptureSessionFailure> {
    let device = open_device(shared).await?;
    let root = device.root();
    // A G713 has the gaming cluster and no `0x1b04` at all: its controls are
    // then simply never divertable.
    let controls = root
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(CaptureError::from)?
        .map(|info| {
            ReprogControlsV4::new(
                Arc::clone(shared.channel()),
                shared.device_index(),
                info.index,
            )
        });
    let table = match &controls {
        Some(rc) => enumerate_controls(rc).await?,
        None => Vec::new(),
    };
    // Subscribed before the first diversion write (see
    // `subscribe_reporting_changes`).
    let reporting_changes = controls.as_ref().map(subscribe_reporting_changes);
    let rollers = arm_rollers(shared, &device).await?;
    let mut armed = ArmedKeys {
        controls,
        table,
        reporting: Vec::new(),
        keys: Arc::default(),
        grid: Arc::default(),
        root,
        channel: Arc::clone(shared.channel()),
        device_index: shared.device_index(),
        gaming: Arc::default(),
        rollers,
        reporting_changes,
        wanted,
    };
    // Arming is the first change, from no key captured to the wanted set.
    if let Err(error) = armed.retarget().await {
        let pending = armed.into_pending(shared);
        return Err(rollback_start(error, pending, registry).await);
    }
    Ok(armed)
}

/// Resolve a dialpad's `0x4610` rollers to its rotary controls. Writes
/// nothing: a roller is diverted only once a binding wants it. `None` when
/// the route is no known dialpad or the device lacks the feature.
async fn arm_rollers(
    shared: &SharedChannel,
    device: &hidpp::device::Device,
) -> Result<Option<ArmedRollers>, CaptureError> {
    let Some(dialpad) = shared.route().dialpad() else {
        return Ok(None);
    };
    let Some(info) = device
        .root()
        .get_feature(MultiRollerFeature::ID)
        .await
        .map_err(CaptureError::from)?
    else {
        warn!("dialpad exposes no 0x4610 MultiRoller — dial and roller stay native");
        return Ok(None);
    };
    let feature = MultiRollerFeature::new(
        Arc::clone(shared.channel()),
        shared.device_index(),
        info.index,
    );
    let mut rollers = BTreeMap::new();
    for (rotary, id) in [
        (Rotary::Dial, dialpad.dial_roller),
        (Rotary::Roller, dialpad.roller_roller),
    ] {
        let id = RollerId(id);
        // The resolution only scales continuous scrolling; an unreadable one
        // degrades to unscaled increments rather than failing the session.
        let per_revolution = match feature.get_roller_capabilities(id).await {
            Ok(caps) => u16::from(caps.increments_per_revolution),
            Err(error) => {
                debug!(?rotary, %error, "roller resolution unreadable");
                0
            }
        };
        rollers.insert(rotary, (id, per_revolution));
    }
    Ok(Some(ArmedRollers {
        feature,
        feature_index: info.index,
        rollers,
        diverted: BTreeSet::new(),
        dispatch: Arc::default(),
    }))
}

/// A dialpad's `0x4610` rollers, and which of its rotary controls the session
/// diverted.
struct ArmedRollers {
    feature: MultiRollerFeature,
    feature_index: u8,
    /// Each rotary control's roller and its increments per revolution.
    rollers: BTreeMap<Rotary, (RollerId, u16)>,
    /// Every rotary control whose diversion may have been written.
    diverted: BTreeSet<Rotary>,
    /// What the report handler dispatches, by the roller id events carry.
    dispatch: Arc<Mutex<BTreeMap<RollerId, (Rotary, u16)>>>,
}

impl ArmedRollers {
    fn dispatch(&self) -> MutexGuard<'_, BTreeMap<RollerId, (Rotary, u16)>> {
        self.dispatch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hand back the rotary controls `wanted` drops, then divert the ones it
    /// adds. A control in both is not written.
    async fn follow(&mut self, wanted: &BTreeSet<Rotary>) -> Result<(), CaptureError> {
        let hand_back: Vec<Rotary> = self.diverted.difference(wanted).copied().collect();
        for rotary in hand_back {
            if let Some(&(id, _)) = self.rollers.get(&rotary) {
                self.dispatch().remove(&id);
                self.feature
                    .set_mode(id, RollerReportingMode::Native)
                    .await
                    .map_err(CaptureError::from)?;
            }
            // Forgotten only once the firmware has it back.
            self.diverted.remove(&rotary);
        }
        let adds: Vec<Rotary> = wanted.difference(&self.diverted).copied().collect();
        for rotary in adds {
            let Some(&(id, per_revolution)) = self.rollers.get(&rotary) else {
                continue;
            };
            // Recorded before the write, so a transport failure still hands
            // it back; dispatched before it, so the first event is not lost.
            self.diverted.insert(rotary);
            self.dispatch().insert(id, (rotary, per_revolution));
            self.feature
                .set_mode(id, RollerReportingMode::Diverted)
                .await
                .map_err(CaptureError::from)?;
        }
        Ok(())
    }

    fn diverted_ids(&self) -> Vec<RollerId> {
        self.diverted
            .iter()
            .filter_map(|rotary| self.rollers.get(rotary).map(|&(id, _)| id))
            .collect()
    }
}

/// Decode a channel message into a [`RollerRotation`] when it is the
/// unsolicited `0x4610` rotation event (function `0`, software id `0`) for
/// `(device_index, feature_index)`.
fn decode_rotation(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<RollerRotation> {
    let header = msg.header();
    (header.device_index == device_index
        && header.feature_index == feature_index
        && header.software_id.to_lo() == 0
        && header.function_id.to_lo() == 0)
        .then(|| RollerRotation::from_payload(&msg.extend_payload()))
}

/// The keys a session dispatches, and which of them are held. The report
/// handler reads it on the channel's read thread while a divert-set change
/// updates it, so the session and the handler share it behind a `Mutex`.
#[derive(Debug, Default)]
struct DivertedKeys {
    /// Every key the session has diverted, and the button it dispatches as.
    buttons: BTreeMap<u16, ButtonId>,
    /// The diverted keys physically held, as of the last report.
    held: BTreeSet<u16>,
}

impl DivertedKeys {
    /// Diff one full diverted-control snapshot into exactly one edge per
    /// physical transition. Unchanged snapshots are deliberately silent.
    fn report(&mut self, cids: &[u16], sink: &mpsc::UnboundedSender<CapturedInput>) {
        for (&cid, &button) in &self.buttons {
            let now = cids.contains(&cid);
            let was = self.held.contains(&cid);
            if now && !was {
                let _ = sink.send(CapturedInput::ButtonDown(button));
            } else if !now && was {
                let _ = sink.send(CapturedInput::ButtonUp(button));
            }
            if now {
                self.held.insert(cid);
            } else {
                self.held.remove(&cid);
            }
        }
    }

    /// Stop dispatching `cid`. A press still held gets no release edge: the
    /// binding change that removed the key also cancels the owner's input
    /// lifecycles.
    fn remove(&mut self, cid: u16) {
        self.buttons.remove(&cid);
        self.held.remove(&cid);
    }
}

/// The firmware writes that take a session from the keys it has diverted to
/// the keys its owner wants. A key in both sets is not written: it stays
/// diverted, so it never runs its native function in between.
#[derive(Debug, Default, PartialEq, Eq)]
struct DivertChange {
    /// Diverted keys the owner no longer wants, to hand back to the firmware.
    hand_back: Vec<u16>,
    /// Wanted keys this keyboard can divert that are not diverted yet.
    divert: Vec<(u16, ButtonId)>,
    /// Diverted keys that stay, with the button each dispatches as now.
    keep: Vec<(u16, ButtonId)>,
}

impl DivertChange {
    fn between(
        diverted: &BTreeMap<u16, ButtonId>,
        wanted: &BTreeMap<u16, ButtonId>,
        table: &[CtrlIdInfo],
    ) -> Self {
        let mut change = Self {
            hand_back: diverted
                .keys()
                .copied()
                .filter(|cid| !wanted.contains_key(cid))
                .collect(),
            ..Self::default()
        };
        for (&cid, &button) in wanted {
            if diverted.contains_key(&cid) {
                change.keep.push((cid, button));
            } else if can_divert(cid, table) {
                change.divert.push((cid, button));
            }
        }
        change
    }

    /// Whether the change writes nothing.
    fn writes_nothing(&self) -> bool {
        self.hand_back.is_empty() && self.divert.is_empty()
    }
}

/// Whether a session may divert `cid` on the keyboard whose `0x1b04` control
/// table is `table`. A key it may not divert is logged and left native, so
/// support degrades per key.
fn can_divert(cid: u16, table: &[CtrlIdInfo]) -> bool {
    if is_reserved_keyboard_control(cid) {
        warn!(
            cid = format_args!("{cid:#06x}"),
            "reserved control requested as a keyboard key — left native"
        );
        return false;
    }
    let divertable = table.iter().any(|c| c.cid == cid && c.is_divertable());
    if !divertable {
        debug!(
            cid = format_args!("{cid:#06x}"),
            "bound key not divertable on this keyboard — left native"
        );
    }
    divertable
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

/// The keypad grid keys a session dispatches, and which of them are held.
/// The raw-report handler reads it on the channel's read thread while a
/// wanted-set change updates it, so it is shared like [`DivertedKeys`]. No
/// firmware state is owned here — these keys report whatever the binding does.
#[derive(Debug, Default)]
struct GridKeys {
    /// Bound grid keys, keyed by the wire index.
    buttons: BTreeMap<u8, ButtonId>,
    /// The bound grid keys physically held, as of the last frame.
    held: BTreeSet<u8>,
}

impl GridKeys {
    /// Diff one vendor frame into edges (see [`emit_grid_edges`]).
    fn report(&mut self, bytes: &[u8], sink: &mpsc::UnboundedSender<CapturedInput>) {
        emit_grid_edges(&mut self.held, bytes, &self.buttons, sink);
    }

    /// Dispatch `wanted` from now on. A held key that left the set is
    /// forgotten without a release edge, as [`DivertedKeys::remove`] does.
    fn follow(&mut self, wanted: &BTreeMap<u8, ButtonId>) {
        self.buttons.clone_from(wanted);
        self.held.retain(|index| wanted.contains_key(index));
    }
}

/// Where a keyboard's M-key and MR features sit in its feature table, or
/// `None` for one it lacks. The G-key row's `0x8010` index travels in its
/// [`GRow`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct GamingFeatures {
    m_keys: Option<u8>,
    macro_record: Option<u8>,
}

/// Who owns a G-series keyboard's `0x8010` G-key row, as far as the session
/// has written it. A write passes through an owed state while it is in
/// flight and stays there when it fails, since a transport failure does not
/// prove the firmware ignored it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum GRow {
    /// The gaming cluster has not been looked up: nothing has asked for it.
    #[default]
    NotLookedUp,
    /// The keyboard has no `0x8010`, so it has no row to take.
    NoGKeys,
    /// The row runs its onboard actions.
    Free { index: u8 },
    /// The take has been written but has not landed.
    TakeOwed { index: u8 },
    /// The firmware reports the row instead of running its onboard actions.
    Taken { index: u8 },
    /// The release has been written but has not landed.
    ReleaseOwed { index: u8 },
}

impl GRow {
    /// The `0x8010` index the row's reports arrive on, once looked up.
    fn index(self) -> Option<u8> {
        match self {
            Self::NotLookedUp | Self::NoGKeys => None,
            Self::Free { index }
            | Self::TakeOwed { index }
            | Self::Taken { index }
            | Self::ReleaseOwed { index } => Some(index),
        }
    }

    /// The row's index from the moment its take is written until its
    /// release is: the firmware has been asked to report the row. Its
    /// reports dispatch from before the take lands, so the first one is not
    /// dropped, and a wake takes it again. A row being handed back does
    /// neither: the owner no longer wants it, and a press it dispatched
    /// would lose its release once the firmware has the row back.
    fn claimed(self) -> Option<u8> {
        match self {
            Self::TakeOwed { index } | Self::Taken { index } => Some(index),
            Self::NotLookedUp | Self::NoGKeys | Self::Free { .. } | Self::ReleaseOwed { .. } => {
                None
            }
        }
    }

    /// The row's index while the firmware may hold it, so teardown hands it
    /// back: exactly the states [`Self::release`] writes from.
    fn owed(self) -> Option<u8> {
        self.release().map(RowWrite::index)
    }

    /// The write that takes the row, or `None` when it is taken or there is
    /// none. An owed row is written again either way.
    fn take(self) -> Option<RowWrite> {
        match self {
            Self::Free { index } | Self::TakeOwed { index } | Self::ReleaseOwed { index } => {
                Some(RowWrite::Take { index })
            }
            Self::NotLookedUp | Self::NoGKeys | Self::Taken { .. } => None,
        }
    }

    /// The write that hands the row back, or `None` when it is free or there
    /// is none.
    fn release(self) -> Option<RowWrite> {
        match self {
            Self::TakeOwed { index } | Self::Taken { index } | Self::ReleaseOwed { index } => {
                Some(RowWrite::Release { index })
            }
            Self::NotLookedUp | Self::NoGKeys | Self::Free { .. } => None,
        }
    }
}

/// One ownership write to a G-key row, from [`GRow::take`] or
/// [`GRow::release`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowWrite {
    Take { index: u8 },
    Release { index: u8 },
}

impl RowWrite {
    fn index(self) -> u8 {
        match self {
            Self::Take { index } | Self::Release { index } => index,
        }
    }

    /// The row from before the write is sent until it lands. It is owed
    /// before the write, so a failed one is still handed back.
    fn owed(self) -> GRow {
        match self {
            Self::Take { index } => GRow::TakeOwed { index },
            Self::Release { index } => GRow::ReleaseOwed { index },
        }
    }

    /// The row once the write has landed.
    fn landed(self) -> GRow {
        match self {
            Self::Take { index } => GRow::Taken { index },
            Self::Release { index } => GRow::Free { index },
        }
    }
}

/// The gaming-cluster keys a session reports, and which of them are held.
/// The report handler reads it on the channel's read thread while a
/// wanted-set change or a reconnect updates it, so it is shared like
/// [`DivertedKeys`].
#[derive(Debug, Default)]
struct GamingKeys {
    /// Who owns the G-key row. Its G-key reports dispatch while it is
    /// [`GRow::claimed`].
    row: GRow,
    /// The M-key and MR feature indices, once looked up.
    features: GamingFeatures,
    /// The keys above the row whose presses dispatch.
    mode_keys: BTreeSet<ModeKey>,
    g_held: GKeys,
    m_held: MKeys,
    mr_held: bool,
    /// Where an interrupted press is reported, once the session listens.
    sink: Option<mpsc::UnboundedSender<CapturedInput>>,
}

impl GamingKeys {
    /// Diff one `0x8010` / `0x8020` / `0x8030` state report into edges, one
    /// per physical transition of a key the session reports.
    fn report(
        &mut self,
        msg: &v20::Message,
        device_index: u8,
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        let header = msg.header();
        if header.device_index != device_index || header.software_id.to_lo() != 0 {
            return;
        }
        let index = Some(header.feature_index);
        let function = header.function_id.to_lo();
        let payload = msg.extend_payload();
        if index == self.row.index() {
            if let Some(GamingGKeysEvent::Held(held)) = GamingGKeysEvent::decode(function, &payload)
            {
                self.on_g_keys(held, sink);
            }
        } else if index == self.features.m_keys {
            if let Some(GamingMKeysEvent::Held(held)) = GamingMKeysEvent::decode(function, &payload)
            {
                self.on_m_keys(held, sink);
            }
        } else if index == self.features.macro_record
            && let Some(MacroRecordEvent::Held(held)) = MacroRecordEvent::decode(function, &payload)
        {
            self.on_macro_record(held, sink);
        }
    }

    fn on_g_keys(&mut self, held: GKeys, sink: &mpsc::UnboundedSender<CapturedInput>) {
        let held = if self.row.claimed().is_some() {
            held
        } else {
            GKeys::empty()
        };
        for key in GKey::ALL {
            let bit = GKeys::from_bits_retain(1 << (key.number() - 1));
            edge(
                self.g_held.contains(bit),
                held.contains(bit),
                ButtonId::g_key(key),
                sink,
            );
        }
        self.g_held = held;
    }

    fn on_m_keys(&mut self, held: MKeys, sink: &mpsc::UnboundedSender<CapturedInput>) {
        for (key, bit) in [
            (ModeKey::M1, MKeys::M1),
            (ModeKey::M2, MKeys::M2),
            (ModeKey::M3, MKeys::M3),
        ] {
            let now = held.contains(bit) && self.mode_keys.contains(&key);
            edge(self.m_held.contains(bit), now, ButtonId::ModeKey(key), sink);
            self.m_held.set(bit, now);
        }
    }

    fn on_macro_record(&mut self, held: bool, sink: &mpsc::UnboundedSender<CapturedInput>) {
        let now = held && self.mode_keys.contains(&ModeKey::MacroRecord);
        edge(
            self.mr_held,
            now,
            ButtonId::ModeKey(ModeKey::MacroRecord),
            sink,
        );
        self.mr_held = now;
    }

    /// Move the G-key row to `row`. A held G-key is forgotten without a
    /// release edge once the row stops dispatching, as
    /// [`DivertedKeys::remove`] forgets a key.
    fn set_row(&mut self, row: GRow) {
        self.row = row;
        if row.claimed().is_none() {
            self.g_held = GKeys::empty();
        }
    }

    /// Report `wanted`'s mode keys from now on. A held key that leaves the
    /// set is forgotten without a release edge, as [`DivertedKeys::remove`]
    /// does.
    fn follow(&mut self, wanted: &BTreeSet<ModeKey>) {
        self.mode_keys.clone_from(wanted);
        for (key, bit) in [
            (ModeKey::M1, MKeys::M1),
            (ModeKey::M2, MKeys::M2),
            (ModeKey::M3, MKeys::M3),
        ] {
            if !wanted.contains(&key) {
                self.m_held.remove(bit);
            }
        }
        self.mr_held &= wanted.contains(&ModeKey::MacroRecord);
    }

    /// Forget every held key after the keyboard power-cycled. It reports the
    /// keys up again only as a state with nothing held, which would be no
    /// edge from a stale held state, so the next press would be swallowed.
    /// A press that was in progress ends without its release action. With
    /// the row taken it is sent even when nothing was held: the keyboard is
    /// back on M1, so the profile the agent selected for G1–G5 starts over.
    fn interrupt(&mut self) {
        let held = !self.g_held.is_empty() || !self.m_held.is_empty() || self.mr_held;
        if (held || self.row.claimed().is_some())
            && let Some(sink) = &self.sink
        {
            let _ = sink.send(CapturedInput::Interrupted);
        }
        self.g_held = GKeys::empty();
        self.m_held = MKeys::empty();
        self.mr_held = false;
    }
}

/// Send the edge of `button` going from `was` to `now` held, if any.
fn edge(was: bool, now: bool, button: ButtonId, sink: &mpsc::UnboundedSender<CapturedInput>) {
    if now && !was {
        let _ = sink.send(CapturedInput::ButtonDown(button));
    } else if was && !now {
        let _ = sink.send(CapturedInput::ButtonUp(button));
    }
}

struct ArmedKeys {
    /// The keyboard's `0x1b04` feature, `None` on one without it.
    controls: Option<ReprogControlsV4>,
    /// The keyboard's `0x1b04` control table, read once at arming, so a
    /// divert-set change writes only the keys that change.
    table: Vec<CtrlIdInfo>,
    /// Every key whose diversion may have been written, with the reporting
    /// state that teardown hands back.
    reporting: Vec<ArmedReporting>,
    /// What the report handler dispatches.
    keys: Arc<Mutex<DivertedKeys>>,
    /// What the raw-report handler dispatches.
    grid: Arc<Mutex<GridKeys>>,
    /// The keyboard's root feature, for looking up the gaming cluster the
    /// first time the owner asks for it.
    root: Arc<RootFeature>,
    /// The session's channel and device, for the gaming-cluster writes.
    channel: Arc<HidppChannel>,
    device_index: u8,
    /// What the report handler dispatches for the gaming cluster, and who
    /// owns the G-key row.
    gaming: Arc<Mutex<GamingKeys>>,
    /// A dialpad's rollers, when the device is one.
    rollers: Option<ArmedRollers>,
    /// Other clients' reporting changes on `controls`, until the running
    /// session takes them.
    reporting_changes: Option<ReportingChangeStream>,
    /// The keys the owner wants captured. [`ArmedCapture::retarget`] marks a
    /// value seen when it applies it.
    wanted: watch::Receiver<KeyboardWanted>,
}

impl ArmedKeys {
    /// The dispatch state, recovered even if a prior holder panicked — every
    /// critical section is panic-free, so the data is consistent.
    fn keys(&self) -> MutexGuard<'_, DivertedKeys> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The grid dispatch state, recovered like [`Self::keys`].
    fn grid(&self) -> MutexGuard<'_, GridKeys> {
        self.grid.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The gaming dispatch state, recovered like [`Self::keys`].
    fn gaming(&self) -> MutexGuard<'_, GamingKeys> {
        self.gaming.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take or hand back the G-key row and follow the cluster keys `wanted`
    /// names. The row is taken only from a keyboard whose column OpenLogi
    /// models whole; a longer one would silence keys nothing can bind.
    async fn follow_gaming(&mut self, wanted: &GamingWanted) -> Result<(), CaptureError> {
        if *wanted != GamingWanted::default() && self.gaming().row == GRow::NotLookedUp {
            let root = Arc::clone(&self.root);
            let index = |id| {
                let root = Arc::clone(&root);
                async move { Ok::<_, CaptureError>(root.get_feature(id).await?.map(|info| info.index)) }
            };
            let g_keys = index(GamingGKeysFeature::ID).await?;
            let features = GamingFeatures {
                m_keys: index(gaming_m_keys::FEATURE_ID).await?,
                macro_record: index(macro_record::FEATURE_ID).await?,
            };
            let mut gaming = self.gaming();
            gaming.features = features;
            gaming.set_row(g_keys.map_or(GRow::NoGKeys, |index| GRow::Free { index }));
        }
        // Mode keys report only while the row is taken, so they can follow
        // the new set ahead of the row's write.
        self.gaming().follow(&wanted.mode_keys);
        let row = self.gaming().row;
        let write = match (wanted.take_g_row, row) {
            (false, _) => row.release(),
            (true, GRow::Free { index }) => {
                let count = self.g_key_feature(index).key_count().await?;
                if usize::from(count) > GKey::ALL.len() {
                    warn!(
                        count,
                        "G-key column longer than OpenLogi models — left onboard"
                    );
                    None
                } else {
                    row.take()
                }
            }
            (true, _) => row.take(),
        };
        if let Some(write) = write {
            let feature = self.g_key_feature(write.index());
            self.gaming().set_row(write.owed());
            match write {
                RowWrite::Take { .. } => feature.take_row().await?,
                RowWrite::Release { .. } => feature.release_row().await?,
            }
            self.gaming().set_row(write.landed());
        }
        if wanted.take_g_row && row == GRow::NoGKeys {
            debug!("G-key row requested on a keyboard without 0x8010 — nothing to take");
        }
        Ok(())
    }

    fn g_key_feature(&self, index: u8) -> GamingGKeysFeature {
        GamingGKeysFeature::new(Arc::clone(&self.channel), self.device_index, index)
    }

    /// Hand back the keys `change` drops, then divert the keys it adds.
    async fn apply(&mut self, change: DivertChange) -> Result<(), CaptureError> {
        // An empty `0x1b04` table never yields a change, so a change always
        // has the feature it writes through.
        let Some(controls) = self.controls.as_ref() else {
            return Ok(());
        };
        for cid in change.hand_back {
            if let Some(position) = self.reporting.iter().position(|r| r.cid == cid) {
                let reporting = self.reporting[position];
                controls
                    .set_cid_reporting_full(cid, reporting.undivert_change())
                    .await?;
                // Forgotten only once the firmware has it back: after a
                // failure, teardown hands it back again.
                self.reporting.remove(position);
            }
            self.keys().remove(cid);
        }
        for (cid, button) in change.divert {
            let original = controls.get_cid_reporting(cid).await?;
            // A transport failure does not prove the firmware rejected the
            // command, so include this CID in rollback before writing.
            self.reporting.push(ArmedReporting::keyboard(cid, original));
            // Dispatched before the write lands, so the key's first diverted
            // report is not dropped.
            self.keys().buttons.insert(cid, button);
            controls
                .set_cid_reporting_full(cid, divert_keyboard_key(original))
                .await?;
        }
        let mut keys = self.keys();
        for (cid, button) in change.keep {
            keys.buttons.insert(cid, button);
        }
        Ok(())
    }
}

impl ArmedCapture for ArmedKeys {
    const NAME: &'static str = "keyboard key";
    const LIVENESS: Liveness = Liveness::Unwatched;

    fn log_active(&self, device_index: u8, wake_rearm: bool) {
        info!(
            index = device_index,
            keys = self.keys().buttons.len(),
            grid_keys = self.grid().buttons.len(),
            g_row = self.gaming().row.owed().is_some(),
            rotary = self.rollers.as_ref().map_or(0, |r| r.diverted.len()),
            wake_rearm,
            "keyboard key capture active"
        );
    }

    fn report_handler(
        &self,
        device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&v20::Message) + Send + Sync + 'static {
        let feature_index = self.controls.as_ref().map(ReprogControlsV4::feature_index);
        let keys = Arc::clone(&self.keys);
        let gaming = Arc::clone(&self.gaming);
        self.gaming().sink = Some(sink.clone());
        let rollers = self
            .rollers
            .as_ref()
            .map(|r| (r.feature_index, Arc::clone(&r.dispatch)));
        move |msg| {
            if let Some((roller_index, dispatch)) = &rollers
                && let Some(rotation) = decode_rotation(msg, device_index, *roller_index)
            {
                let target = dispatch
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&rotation.roller)
                    .copied();
                if let Some((rotary, per_revolution)) = target
                    && rotation.delta != 0
                {
                    let _ = sink.send(CapturedInput::Rotation {
                        rotary,
                        increments: i16::from(rotation.delta),
                        per_revolution,
                    });
                }
                return;
            }
            gaming
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .report(msg, device_index, &sink);
            let Some(RawControlEvent::DivertedButtons(cids)) = feature_index
                .and_then(|index| reprog_controls::decode_event(msg, device_index, index))
            else {
                return;
            };
            // Recover the guard even if a prior holder panicked — every
            // critical section is panic-free, so the data is consistent.
            keys.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .report(&cids, &sink);
        }
    }

    fn raw_report_handler(
        &self,
        _device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&[u8]) + Send + Sync + 'static {
        let grid = Arc::clone(&self.grid);
        move |bytes| {
            // Recovered like the control handler's guard above.
            grid.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .report(bytes, &sink);
        }
    }

    fn reset_input_state(&self) {
        self.gaming().interrupt();
    }

    async fn rearm(&self) -> bool {
        if let Some(controls) = &self.controls {
            for &reporting in &self.reporting {
                if let Err(e) = controls
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
        if let Some(rollers) = &self.rollers {
            for id in rollers.diverted_ids() {
                if let Err(e) = rollers
                    .feature
                    .set_mode(id, RollerReportingMode::Diverted)
                    .await
                {
                    warn!(
                        roller = id.0,
                        error = %e,
                        "re-divert after wake failed — roller stays native until next wake"
                    );
                }
            }
        }
        // The keyboard forgets a taken row when it power-cycles. Left onboard,
        // every G-key would run its onboard action instead of its binding, so
        // a failed retake ends the session and capture arms again.
        let retake = self.gaming().row.claimed();
        if let Some(index) = retake
            && let Err(error) = self.g_key_feature(index).take_row().await
        {
            warn!(%error, "retaking the G-key row after wake failed");
            return false;
        }
        true
    }

    async fn retarget_requested(&self) {
        // A clone starts at the version this session last applied, so waiting
        // on it leaves that version for `retarget` to mark seen.
        let mut request = self.wanted.clone();
        if request.changed().await.is_err() {
            // The owner is gone, and its shutdown request ends the session.
            std::future::pending::<()>().await;
        }
    }

    async fn retarget(&mut self) -> Result<(), CaptureError> {
        let wanted = self.wanted.borrow_and_update().clone();
        self.grid().follow(&wanted.grid);
        self.follow_gaming(&wanted.gaming).await?;
        let change = DivertChange::between(&self.keys().buttons, &wanted.controls, &self.table);
        if !change.writes_nothing() {
            debug!(
                hand_back = change.hand_back.len(),
                divert = change.divert.len(),
                "keyboard divert set changed"
            );
        }
        self.apply(change).await?;
        if let Some(rollers) = self.rollers.as_mut() {
            return rollers.follow(&wanted.rotary).await;
        }
        if !wanted.rotary.is_empty() {
            debug!("rotary controls requested on a device without dialpad rollers — skipped");
        }
        Ok(())
    }

    fn armed_reporting(&self) -> Option<(&ReprogControlsV4, &[ArmedReporting])> {
        let controls = self.controls.as_ref()?;
        (!self.reporting.is_empty()).then_some((controls, self.reporting.as_slice()))
    }

    fn take_reporting_changes(&mut self) -> Option<ReportingChangeStream> {
        // Kept while no key is diverted too: a later divert-set change may
        // divert one.
        self.reporting_changes.take()
    }

    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        let g_keys = self.gaming().row.owed().map(GKeysRestore::new);
        let reprog = self
            .controls
            .as_ref()
            .and_then(|controls| ReprogRestore::new(controls.feature_index(), self.reporting));
        let rollers = self
            .rollers
            .and_then(|r| RollerRestore::new(r.feature_index, r.diverted_ids()));
        PendingCaptureRestore::new(retired, reprog, None, None, None, g_keys, rollers)
    }
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

    /// A gaming state report on feature `index`, as the keyboard sends it.
    fn gaming_report(index: u8, held: &[u8]) -> v20::Message {
        let mut payload = [0u8; 16];
        payload[..held.len()].copy_from_slice(held);
        v20::Message::Long(
            v20::MessageHeader {
                device_index: 0xff,
                feature_index: index,
                function_id: hidpp::nibble::U4::from_lo(0),
                software_id: hidpp::nibble::U4::from_lo(0),
            },
            payload,
        )
    }

    const G: u8 = 3;
    const M: u8 = 4;
    const MR: u8 = 5;

    fn cluster(
        row: GRow,
        mode_keys: &[ModeKey],
    ) -> (GamingKeys, mpsc::UnboundedReceiver<CapturedInput>) {
        let (sink, inputs) = mpsc::unbounded_channel();
        let mut keys = GamingKeys {
            features: GamingFeatures {
                m_keys: Some(M),
                macro_record: Some(MR),
            },
            sink: Some(sink),
            ..GamingKeys::default()
        };
        keys.set_row(row);
        keys.follow(&mode_keys.iter().copied().collect());
        (keys, inputs)
    }

    fn report(keys: &mut GamingKeys, index: u8, held: &[u8]) {
        let sink = keys.sink.clone().expect("cluster has a sink");
        keys.report(&gaming_report(index, held), 0xff, &sink);
    }

    /// Each row write is owed from before it is sent, so a session torn
    /// down while the write waits, or after it failed, still hands the row
    /// back.
    #[test]
    fn a_row_write_is_owed_back_from_before_it_is_sent() {
        let take = GRow::Free { index: G }
            .take()
            .expect("a free row can be taken");
        assert_eq!(take.owed().owed(), Some(G));
        assert_eq!(take.landed(), GRow::Taken { index: G });
        let release = take
            .landed()
            .release()
            .expect("a taken row can be handed back");
        assert_eq!(release.owed().owed(), Some(G));
        assert_eq!(release.landed(), GRow::Free { index: G });
    }

    /// A failed write leaves the row owed the way it was going, since a
    /// transport failure does not prove the firmware ignored it. A take
    /// owed still dispatches, is taken again on wake, and is written again;
    /// a release owed does neither, and is written again by teardown or the
    /// next change. Neither is mistaken for the other.
    #[test]
    fn a_failed_row_write_stays_owed_the_way_it_was_going() {
        let take = Some(RowWrite::Take { index: G });
        let release = Some(RowWrite::Release { index: G });
        // The row, then what it claims (dispatch and wake), what teardown
        // hands back, and the take and release writes from it.
        let table = [
            (GRow::NotLookedUp, None, None, None, None),
            (GRow::NoGKeys, None, None, None, None),
            (GRow::Free { index: G }, None, None, take, None),
            (GRow::TakeOwed { index: G }, Some(G), Some(G), take, release),
            (GRow::Taken { index: G }, Some(G), Some(G), None, release),
            (GRow::ReleaseOwed { index: G }, None, Some(G), take, release),
        ];
        for (row, claimed, owed, take, release) in table {
            assert_eq!(
                (row.claimed(), row.owed(), row.take(), row.release()),
                (claimed, owed, take, release),
                "{row:?}"
            );
        }
    }

    /// G-key reports dispatch from the moment the take is written, so the
    /// first report is not dropped while the write waits for its reply.
    /// They stop when the release is written: a key held then is forgotten
    /// without a release edge, and a press the firmware reports before the
    /// release lands starts nothing it could no longer end.
    #[test]
    fn g_keys_dispatch_from_the_take_write_to_the_release_write() {
        let (mut keys, mut inputs) = cluster(GRow::Free { index: G }, &[]);
        report(&mut keys, G, &[0b001]);
        let take = keys.row.take().expect("a free row can be taken");
        keys.set_row(take.owed());
        report(&mut keys, G, &[0b010]);
        keys.set_row(take.landed());
        report(&mut keys, G, &[0b000]);
        report(&mut keys, G, &[0b100]);
        let release = keys.row.release().expect("a taken row can be handed back");
        keys.set_row(release.owed());
        report(&mut keys, G, &[0b000]);
        report(&mut keys, G, &[0b001]);
        keys.set_row(release.landed());
        report(&mut keys, G, &[0b000]);
        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G2)),
                CapturedInput::ButtonUp(ButtonId::g_key(GKey::G2)),
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G3)),
            ]
        );
    }

    #[test]
    fn g_key_masks_emit_one_balanced_edge_per_key() {
        let (mut keys, mut inputs) = cluster(GRow::Taken { index: G }, &[]);
        report(&mut keys, G, &[0b0_0001]);
        report(&mut keys, G, &[0b0_0001]);
        report(&mut keys, G, &[0b1_0001]);
        // Bits past G5 belong to no modelled key.
        report(&mut keys, G, &[0b0_0000, 0x01]);
        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G1)),
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G5)),
                CapturedInput::ButtonUp(ButtonId::g_key(GKey::G1)),
                CapturedInput::ButtonUp(ButtonId::g_key(GKey::G5)),
            ]
        );
    }

    #[test]
    fn an_untaken_row_and_unwanted_mode_keys_report_nothing() {
        let (mut keys, mut inputs) = cluster(GRow::Free { index: G }, &[ModeKey::M2]);
        report(&mut keys, G, &[0b1]);
        report(&mut keys, M, &[0b101]);
        report(&mut keys, MR, &[1]);
        report(&mut keys, M, &[0b111]);
        report(&mut keys, M, &[0]);
        // Another feature's report never decodes as a gaming key.
        report(&mut keys, 9, &[0b1]);
        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::ModeKey(ModeKey::M2)),
                CapturedInput::ButtonUp(ButtonId::ModeKey(ModeKey::M2)),
            ]
        );
    }

    /// A keyboard that power-cycles with a G-key held reports it up only as
    /// a state with nothing held. Without forgetting the held state on
    /// reconnect, the next press of that key would be no edge and its
    /// action would not run.
    #[test]
    fn a_reconnect_interrupts_held_keys_and_the_next_press_goes_down() {
        let (mut keys, mut inputs) = cluster(GRow::Taken { index: G }, &[ModeKey::MacroRecord]);
        report(&mut keys, G, &[0b10]);
        report(&mut keys, MR, &[1]);
        keys.interrupt();
        report(&mut keys, G, &[0b10]);
        report(&mut keys, MR, &[1]);
        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G2)),
                CapturedInput::ButtonDown(ButtonId::ModeKey(ModeKey::MacroRecord)),
                CapturedInput::Interrupted,
                CapturedInput::ButtonDown(ButtonId::g_key(GKey::G2)),
                CapturedInput::ButtonDown(ButtonId::ModeKey(ModeKey::MacroRecord)),
            ]
        );
        // Nothing held, nothing to interrupt.
        keys.set_row(GRow::Free { index: G });
        keys.follow(&BTreeSet::new());
        keys.interrupt();
        assert_eq!(drain(&mut inputs), vec![]);
        // With the row taken a wake is reported even with nothing held: the
        // keyboard is back on M1, so the agent's selected profile starts over.
        keys.set_row(GRow::Taken { index: G });
        keys.interrupt();
        assert_eq!(drain(&mut inputs), vec![CapturedInput::Interrupted]);
    }

    #[test]
    fn keyboard_snapshots_emit_balanced_edges_without_duplicates() {
        let mut keys = DivertedKeys {
            buttons: BTreeMap::from([
                (0x00d4, ButtonId::control(0x00d4)),
                (0x0103, ButtonId::control(0x0103)),
            ]),
            held: BTreeSet::new(),
        };
        let (sink, mut inputs) = mpsc::unbounded_channel();

        keys.report(&[0x00d4], &sink);
        keys.report(&[0x00d4], &sink);
        keys.report(&[0x00d4, 0x0103], &sink);
        keys.report(&[0x0103], &sink);
        keys.report(&[], &sink);

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

    /// A key removed while held is forgotten: its next snapshot emits
    /// nothing, and a later divert starts it released.
    #[test]
    fn a_removed_key_emits_no_edges() {
        let mut keys = DivertedKeys {
            buttons: BTreeMap::from([(0x00d4, ButtonId::control(0x00d4))]),
            held: BTreeSet::new(),
        };
        let (sink, mut inputs) = mpsc::unbounded_channel();
        keys.report(&[0x00d4], &sink);
        keys.remove(0x00d4);
        keys.report(&[], &sink);
        keys.buttons.insert(0x00d4, ButtonId::control(0x00d4));
        keys.report(&[0x00d4], &sink);

        assert_eq!(
            std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                CapturedInput::ButtonDown(ButtonId::control(0x00d4)),
                CapturedInput::ButtonDown(ButtonId::control(0x00d4)),
            ]
        );
    }

    fn control(cid: u16, divertable: bool) -> CtrlIdInfo {
        let flags = if divertable {
            reprog_controls::CidFlags::DIVERTABLE
        } else {
            reprog_controls::CidFlags::empty()
        };
        CtrlIdInfo {
            cid,
            task_id: 0,
            flags: flags.bits(),
        }
    }

    fn keys(cids: &[u16]) -> BTreeMap<u16, ButtonId> {
        cids.iter()
            .map(|&cid| (cid, ButtonId::control(cid)))
            .collect()
    }

    /// Only the keys that leave or join the set are written. A key the
    /// keyboard cannot divert, or a reserved one, never enters the change.
    #[test]
    fn a_divert_change_writes_only_the_keys_that_change() {
        let table = [
            control(0x00e7, true),
            control(0x00e8, true),
            control(0x00e9, true),
            control(0x00ea, false),
            control(0x00d1, true),
        ];
        let change = DivertChange::between(
            &keys(&[0x00e7, 0x00e8]),
            &keys(&[0x00e7, 0x00e9, 0x00ea, 0x00d1, 0x00fe]),
            &table,
        );
        assert_eq!(
            change,
            DivertChange {
                hand_back: vec![0x00e8],
                divert: vec![(0x00e9, ButtonId::control(0x00e9))],
                keep: vec![(0x00e7, ButtonId::control(0x00e7))],
            }
        );

        let unchanged = DivertChange::between(&keys(&[0x00e7]), &keys(&[0x00e7]), &table);
        assert!(unchanged.writes_nothing());
        assert!(DivertChange::between(&keys(&[]), &keys(&[]), &table).writes_nothing());
        assert_eq!(
            DivertChange::between(&keys(&[0x00e7, 0x00e8]), &keys(&[]), &table).hand_back,
            [0x00e7, 0x00e8]
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

    /// A grid key that leaves the wanted set while held is forgotten: its
    /// release frame emits nothing, and a key that stays keeps its state.
    #[test]
    fn a_grid_key_removed_while_held_emits_no_release() {
        let mut grid = GridKeys::default();
        grid.follow(&BTreeMap::from([
            (2, ButtonId::grid(GridKey::TopMiddle)),
            (3, ButtonId::grid(GridKey::TopRight)),
        ]));
        let (sink, mut inputs) = mpsc::unbounded_channel();

        grid.report(&grid_pdu(0x01, &[2, 3]), &sink);
        grid.follow(&BTreeMap::from([(3, ButtonId::grid(GridKey::TopRight))]));
        grid.report(&grid_pdu(0x02, &[]), &sink);

        assert_eq!(
            drain(&mut inputs),
            vec![
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::TopMiddle)),
                CapturedInput::ButtonDown(ButtonId::grid(GridKey::TopRight)),
                CapturedInput::ButtonUp(ButtonId::grid(GridKey::TopRight)),
            ]
        );
        assert_eq!(grid.held, BTreeSet::default());
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

    /// A wake that cannot take the G-key row back leaves every G-key on its
    /// onboard action, so the session has to end and arm again. A row whose
    /// release is still owed is not taken again.
    #[tokio::test]
    async fn a_g_key_row_that_cannot_be_retaken_after_wake_ends_the_session() {
        use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
        use std::sync::atomic::{AtomicBool, Ordering};
        const TAKE_ROW: u8 = 2;
        let take_fails = Arc::new(AtomicBool::new(false));
        let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder({
            let take_fails = Arc::clone(&take_fails);
            move |request| {
                let mut response = vec![0; 20];
                response[..4].copy_from_slice(&request[..4]);
                response[0] = 0x11;
                match (request[2], request[3] >> 4) {
                    (0, 1) => response[4] = 4,
                    (G, TAKE_ROW) if take_fails.load(Ordering::Relaxed) => {
                        response[2] = 0xff;
                        response[3] = G;
                        response[4] = request[3];
                        response[5] = 2;
                    }
                    (G, TAKE_ROW) => {}
                    _ => panic!("unexpected rearm request: {request:02x?}"),
                }
                Some(response)
            }
        });
        let channel = scripted_channel(raw).await;
        let device = hidpp::device::Device::new(Arc::clone(&channel), 0xff)
            .await
            .expect("the scripted keyboard answers");
        let armed = ArmedKeys {
            controls: None,
            table: Vec::new(),
            reporting: Vec::new(),
            keys: Arc::default(),
            grid: Arc::default(),
            root: device.root(),
            channel,
            device_index: 0xff,
            gaming: Arc::default(),
            rollers: None,
            reporting_changes: None,
            wanted: watch::channel(KeyboardWanted::default()).1,
        };
        armed.gaming().set_row(GRow::Taken { index: G });

        assert!(armed.rearm().await, "a retaken row keeps the session");
        take_fails.store(true, Ordering::Relaxed);
        assert!(!armed.rearm().await, "a row left onboard ends the session");
        armed.gaming().set_row(GRow::ReleaseOwed { index: G });
        assert!(armed.rearm().await, "a row owed back is not retaken");
    }
}

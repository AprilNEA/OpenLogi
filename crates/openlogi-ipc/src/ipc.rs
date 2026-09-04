//! tarpc service contract between the GUI (client) and the agent (server).
//!
//! tarpc generates the `AgentClient` and the `serve` glue from this trait. tarpc
//! is strict request/response — no server push — so anything the agent needs to
//! *tell* a client is shaped as a request the client leaves open, and the agent
//! holds it until there is something to say or its hold window elapses.
//! [`Agent::observe`] is that channel for state: it answers the moment the
//! agent's observable state changes, and answers with the whole of it, so a
//! client needs no subscription and no replay to converge. `next_pairing` and
//! `next_action_ring` are the same shape for their own streams.

use std::collections::BTreeMap;
use std::time::Duration;

use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{ActionRingIcon, ActionRingSlot};
use openlogi_core::config::Lighting;
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::{
    DeviceRoute, Dpi, DpiInfo, LightCommand, PairingError, PasskeyMethod, ReceiverSelector,
    SmartShiftStatus, WriteError,
};
use serde::{Deserialize, Serialize};
pub use succession::Identity;

/// Wire-protocol version. Bumped only on a breaking change to the types below —
/// independent of the crate version. The GUI checks it via
/// [`Agent::protocol_version`] on connect and refuses to drive a mismatch
/// (transient only: both binaries ship in one `.app` and update atomically).
///
/// v2: `AgentStatus::inventory_ready` added.
/// v3: `inventory_ready` widened to [`InventoryHealth`] (adds `Unavailable`).
/// v4: [`Agent::snapshot`] added for atomic status + inventory polling.
/// v5: [`PairingUpdate::Failed`] carries a typed [`PairingFailure`].
/// v6: `Capabilities::scroll_inversion` added.
/// v7: pairing commands return typed acceptance errors.
/// v8: [`WriteError`] carries typed HID++ operation failures.
/// v9: `poll_event_monitor` appended + [`MonitorEvent`] (live event monitor).
/// v10: `Capabilities::hires_wheel` appended.
/// v11: standalone raw-HID inventory, camera state, and light methods appended.
/// v12: standalone registry model identity appended to `StandaloneDevice`.
/// v13: `Capabilities::thumbwheel` appended.
/// v14: Actions Ring RPCs and haptic capability fields appended.
/// v15: custom Actions Ring presentation icons and locale appended.
/// v16: `ActionRingPresentation::literal` inserted (custom labels render
///      verbatim instead of passing through localization).
/// v17: `reload_config` returns a typed parse/validation result.
/// v18: `identity` appended (supervised helpers bind to one agent run).
/// v19: `observe` appended (level-triggered state channel; see [`Agent::observe`]).
/// v20: `AgentSnapshot::pairing` appended — the pairing session is state now,
///      not a stream of steps (see [`PairingPhase`]).
/// v21: `observe_action_ring` appended — the showing ring is state too (see
///      [`RingObservation`]).
/// v22: DPI scalar values use the validated [`Dpi`] type end to end.
/// v23: SmartShift writes carry one typed [`SmartShiftStatus`] value.
/// v24: `StandaloneDevice::registry_model_id` is always encoded (bincode fix).
/// v25: `AgentStatus::input_monitoring_granted` appended.
/// v26: `AgentStatus::hid_open_failures` appended.
/// v27: `AgentSnapshot::foreground` appended — the frontmost application the
///      agent matches per-app profiles against, plus the ones it saw recently.
/// v28: `Action::HoldShortcut` appended for lifecycle-held keyboard output.
/// v29: `Agent::declare_client` + [`ClientKind`] appended — typed demand for
///      the macOS dormancy gate.
/// v30: `Capabilities::gaming_buttons` appended — HID++ `0x8100`/`0x8110`
///      G-series button remapping (no `0x1b04`).
pub const PROTOCOL_VERSION: u32 = 30;

/// Environment variable through which the agent hands a supervised helper the
/// run token it will serve, so the helper knows which agent it belongs to
/// before it has spoken to one. Absent for a hand-started helper, which then
/// adopts whichever run answers first.
pub const RUN_ENV: &str = "OPENLOGI_RUN";

/// Why the agent refused to adopt the config currently on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReloadError {
    /// Actionable config error including path and TOML location when known.
    pub message: String,
}

/// Where the agent's device enumeration stands. The distinction matters
/// because an empty inventory list is ambiguous on its own: the GUI must keep
/// its scanning state while the answer simply isn't in yet, show the empty
/// state only for a *completed* scan that found nothing, and surface an error —
/// rather than scan forever — when enumeration itself is broken.
///
/// bincode encodes the variant *index*, so variants are append-only, like the
/// [`Agent`] trait methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InventoryHealth {
    /// The first enumeration hasn't completed yet — the device set is unknown.
    Scanning,
    /// At least one enumeration has completed; the inventory list is
    /// authoritative (an empty list really means no devices).
    Ready,
    /// Enumeration has never succeeded and has stopped being retried as a
    /// startup condition (the HID backend is broken or inaccessible, or the
    /// watcher died). Details are in the agent log.
    Unavailable,
}

/// Agent health the GUI surfaces: the Accessibility gate, whether the hook is
/// live, the autostart toggle state, and enumeration progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "AgentStatus is a serialized health DTO; bools keep the IPC shape explicit"
)]
pub struct AgentStatus {
    pub accessibility_granted: bool,
    pub hook_installed: bool,
    pub launch_at_login: bool,
    /// See [`InventoryHealth`]; the GUI picks its empty-state body off this.
    pub inventory: InventoryHealth,
    pub protocol_version: u32,
    pub agent_version: String,
    /// Whether the agent process holds Input Monitoring (HID) access.
    pub input_monitoring_granted: bool,
    /// Whether the last enumeration tick failed to open at least one HID++
    /// node. Paired with [`Self::input_monitoring_granted`] it distinguishes
    /// a missing grant from an exclusive open or a stale permission session.
    pub hid_open_failures: bool,
}

/// Status and inventory as one poll result. Kept together so the GUI never
/// pairs inventory readiness from one orchestrator state with the inventory
/// list from another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub status: AgentStatus,
    pub inventory: Vec<DeviceInventory>,
    /// Recognized standalone raw-HID devices, kept separate from receiver
    /// pairing inventories.
    pub standalone: Vec<StandaloneDevice>,
    /// Whether at least one host camera stream is currently in use.
    /// Runtime-only state used by camera-linked light rendering.
    pub camera_active: bool,
    /// The pairing session the agent has open, if any. See [`PairingPhase`].
    pub pairing: Option<PairingPhase>,
    /// Which application per-app profiles are resolving against. See
    /// [`ForegroundApps`].
    pub foreground: ForegroundApps,
}

/// The application the agent currently resolves per-app profiles against, and
/// the ones it recently saw in front.
///
/// `recent` is here because a client cannot produce these identifiers itself.
/// They come from four incompatible namespaces — macOS bundle ids, X11
/// `WM_CLASS`, Wayland `app_id`, Windows executable paths — and only the agent
/// holds the one that its matcher will actually compare. Enumerating installed
/// applications in the GUI would produce plausible strings that miss. A client
/// offering "make a profile for…" therefore picks from this list rather than
/// from the host.
///
/// It also answers the case [`Self::current`] cannot: while a client's own
/// window is in front, *it* is the foreground application, so the app the user
/// means is the previous entry, not the current one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundApps {
    /// The application whose profile is live right now, or `None` when nothing
    /// is frontmost or the platform cannot say (a pure-Wayland session with no
    /// usable backend). OpenLogi's own processes are reported here like any
    /// other application — this is the matcher's view, not a filtered one.
    pub current: Option<ForegroundApp>,
    /// The most recently frontmost applications, newest first, deduplicated by
    /// identifier and capped at [`RECENT_APPS`]. Excludes OpenLogi's own
    /// processes, which are never a sensible profile target. Includes
    /// [`Self::current`] when it is not one of them.
    pub recent: Vec<ForegroundApp>,
}

/// How many applications [`ForegroundApps::recent`] remembers: enough to fill a
/// picker with what the user was just doing, few enough that the list stays a
/// rounding error in every observation.
pub const RECENT_APPS: usize = 12;

/// Where a pairing session stands.
///
/// State, not a stream of steps, and the GUI renders it directly. That is what
/// makes an agent restart mid-session self-healing: the replacement agent has no
/// session, so a window left on "Searching…" resolves itself instead of needing
/// a terminal event synthesized on its behalf.
///
/// A terminal phase ([`Self::Paired`], [`Self::Failed`]) stays until the session
/// is cancelled or a new one starts, so a result cannot fall between two
/// observations the way an event can.
///
/// bincode encodes the variant *index*, so variants are append-only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingPhase {
    /// Discovery (Bolt) or the pairing lock (Unifying) is open.
    Searching,
    /// Bolt: the devices discovered so far, in discovery order.
    Found(Vec<FoundDevice>),
    /// A discovered device was picked; the agent is driving the receiver.
    Pairing,
    /// Bolt: the device asks the user to authenticate with a passkey.
    Passkey(PasskeyMethod),
    /// A device paired into `slot`.
    Paired { slot: u8 },
    /// The session ended without pairing.
    Failed(PairingFailure),
}

/// How long the agent holds an [`Agent::observe`] request that has nothing to
/// report before answering with the unchanged state. Exported so a client can
/// set its request deadline above it — tarpc cancels a handler whose deadline
/// passes, so a shorter deadline would kill the hold instead of waiting it out.
pub const OBSERVE_HOLD: Duration = Duration::from_secs(20);

/// Monotonic stamp on the agent's observable state, bumped only when something
/// in it actually changes.
///
/// `0` is the client's "I have seen nothing" sentinel and the agent's cell
/// starts at 1, so a first [`Agent::observe`] answers immediately instead of
/// waiting out the hold for a change that already happened.
pub type Generation = u64;

/// The agent's observable state together with the generation it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// See [`Generation`]. Pass it back to the next [`Agent::observe`].
    pub generation: Generation,
    /// The complete state at that generation — never a delta.
    pub snapshot: AgentSnapshot,
}

/// A nearby unpaired device surfaced during Bolt discovery, in the minimal form
/// the GUI needs: a name to show and the address to pair by. The agent keeps the
/// full `openlogi_hid::DiscoveredDevice` (kind, auth bits) internally, keyed by
/// this address, so the wire form needs neither the non-serializable device-kind
/// nor the auth bitfield.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundDevice {
    pub address: [u8; 6],
    pub name: String,
}

/// Terminal failure reason for a pairing session.
///
/// Kept typed across the agent↔GUI boundary so the GUI can choose recovery UI,
/// telemetry, and localized copy without matching human-readable strings.
///
/// bincode encodes the variant *index*, so variants are append-only, like the
/// [`Agent`] trait methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingFailure {
    /// The HID transport returned an error.
    Hid { message: String },
    /// No connected receiver supports pairing.
    ReceiverNotFound,
    /// HID++ receiver register access failed.
    Register { message: String },
    /// The device or receiver did not complete pairing before its deadline.
    Timeout,
    /// The receiver reported a protocol-level pairing error code.
    Device { code: u8 },
    /// The user cancelled the pairing session.
    Cancelled,
    /// The agent could not obtain exclusive receiver ownership for pairing.
    ReceiverBusy,
    /// The pairing watcher is unavailable inside the agent process.
    WatcherUnavailable,
    /// The background agent restarted during an active pairing session.
    ///
    /// No longer produced by the agent: a restart is now visible as the absence
    /// of a session in [`PairingPhase`], so nothing has to synthesize this. The
    /// GUI still reports it locally when a pairing command cannot be delivered.
    AgentRestarted,
    /// The agent could not store its exclusive receiver ownership lease.
    ReceiverAccessUnavailable,
    /// A pairing session is already active.
    AlreadyActive,
    /// The GUI asked to pair an address that is not in the current discovery cache.
    UnknownDevice,
    /// There is no pairing session to receive this command.
    NoActiveSession,
}

/// Immediate command-acceptance failure for pairing RPCs.
///
/// Progress after a command is accepted still arrives through
/// [`PairingUpdate`]. This type only covers failures that prevent the agent from
/// accepting the command in the first place.
///
/// bincode encodes the variant *index*, so variants are append-only, like the
/// [`Agent`] trait methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingCommandError {
    /// A pairing session is already active.
    AlreadyActive,
    /// The agent could not obtain exclusive receiver ownership for pairing.
    ReceiverBusy,
    /// The pairing watcher is unavailable inside the agent process.
    WatcherUnavailable,
    /// The GUI asked to pair an address that is not in the current discovery cache.
    UnknownDevice,
    /// There is no pairing session to receive this command.
    NoActiveSession,
}

impl From<PairingCommandError> for PairingFailure {
    fn from(error: PairingCommandError) -> Self {
        match error {
            PairingCommandError::AlreadyActive => Self::AlreadyActive,
            PairingCommandError::ReceiverBusy => Self::ReceiverBusy,
            PairingCommandError::WatcherUnavailable => Self::WatcherUnavailable,
            PairingCommandError::UnknownDevice => Self::UnknownDevice,
            PairingCommandError::NoActiveSession => Self::NoActiveSession,
        }
    }
}

impl From<PairingError> for PairingFailure {
    fn from(error: PairingError) -> Self {
        match error {
            PairingError::Hid(message) => Self::Hid { message },
            PairingError::ReceiverNotFound => Self::ReceiverNotFound,
            PairingError::Register(message) => Self::Register { message },
            PairingError::Timeout => Self::Timeout,
            PairingError::Device(code) => Self::Device { code },
            PairingError::Cancelled => Self::Cancelled,
            // The public agent API prevents this library-boundary rejection;
            // retain the existing wire enum if an in-process caller violates it.
            PairingError::UnsupportedCommand => Self::Hid {
                message: "pairing command is not supported by the active receiver".into(),
            },
            // Carried as the generic transport-failure message so the wire
            // format stays unchanged (PairingFailure variants are append-only).
            PairingError::MalformedNotification(what) => Self::Hid {
                message: format!("malformed pairing notification ({what})"),
            },
        }
    }
}

/// One step of a pairing session, streamed to the GUI via [`Agent::next_pairing`].
/// Mirrors `openlogi_hid::PairingEvent` but in a wire-safe form — the discovered
/// device collapses to [`FoundDevice`] and terminal failures to
/// [`PairingFailure`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PairingUpdate {
    /// Discovery (Bolt) / the pairing lock (Unifying) is open.
    Searching,
    /// Bolt only: a nearby unpaired device was discovered.
    DeviceFound(FoundDevice),
    /// Bolt only: the device asks the user to authenticate with a passkey.
    Passkey(PasskeyMethod),
    /// A device paired into `slot`.
    Paired { slot: u8 },
    /// The flow ended without pairing a device.
    Failed(PairingFailure),
}

/// One input event the agent's mouse hook observed, streamed to the GUI's live
/// event monitor via [`Agent::poll_event_monitor`]. Pointer-move events are
/// deliberately excluded — they would flood the buffer — so this is the
/// button/scroll/interrupt view of what OpenLogi's hook actually receives.
///
/// bincode encodes the variant *index*, so variants are append-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MonitorEvent {
    /// A mouse button changed state. `button` is the display label (e.g. `Back`).
    Button { button: String, pressed: bool },
    /// A scroll-wheel tick; positive `delta_x` = right, positive `delta_y` = down.
    Scroll { delta_x: f32, delta_y: f32 },
    /// The OS interrupted capture (the tap was disabled by a timeout or by
    /// competing user input). Surfaced because it explains a momentary gap.
    CaptureInterrupted,
}

/// Non-executable presentation data for one populated Actions Ring slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRingPresentation {
    /// Localization key or user-defined label shown by the overlay.
    pub label: String,
    /// When set, `label` is user-authored free text and renders verbatim —
    /// without this a custom label that collides with a localization key
    /// ("Copy") would be translated on non-English locales.
    pub literal: bool,
    /// Fully resolved icon; the overlay does not need the executable action.
    pub icon: ActionRingIcon,
}

/// One request for the overlay helper to display an Actions Ring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRingInvocation {
    /// Opaque session identity used by hover/activate/cancel commands.
    pub session_id: u64,
    /// Read-only presentation snapshots. Executable actions never cross into
    /// the overlay helper and remain in the agent-owned session.
    pub slots: BTreeMap<ActionRingSlot, ActionRingPresentation>,
    /// Configured UI locale, or `None` to follow the overlay host's system locale.
    pub language: Option<String>,
}

/// The ring the overlay should be showing, stamped with its generation.
///
/// Its own cell rather than a field of [`AgentSnapshot`]: the overlay is a
/// different observer with a different working set, and waking it on every
/// device hotplug to hand it a full device inventory would be wrong for a
/// helper whose only job is to paint a ring the instant a button goes down.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingObservation {
    /// See [`Generation`]. Pass it back to the next [`Agent::observe_action_ring`].
    pub generation: Generation,
    /// The ring to show, or `None` for none — which is also how a dismissal
    /// arrives, so there is no "closed" message to invent.
    pub invocation: Option<ActionRingInvocation>,
}

/// Why an Actions Ring interaction command was rejected.
///
/// Variants are append-only because this enum crosses bincode IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionRingCommandError {
    /// The session was replaced, cancelled, expired, or never existed.
    SessionNotFound,
    /// The selected position has no action.
    SlotEmpty,
}

/// What kind of client a connection is, declared through
/// [`Agent::declare_client`] right after the version handshake.
///
/// Variants are append-only because this enum crosses bincode IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKind {
    /// The desktop app. The only kind whose declaration arms a dormant agent.
    Gui,
    /// The `openlogi` CLI reading a snapshot; served without arming.
    Cli,
    /// The Actions Ring overlay helper; served without arming — one that
    /// connects on its own is an orphan of a previous run.
    Overlay,
}

#[tarpc::service]
pub trait Agent {
    /// Wire-protocol version, for the connect handshake.
    ///
    /// Method *order* is part of the wire format: tarpc generates one request
    /// enum from this trait and bincode encodes the variant index, so this
    /// method must stay **first** — and new methods must be appended at the
    /// end, never inserted — or the handshake itself stops decoding across a
    /// version skew and a mismatch can no longer be detected and reported.
    /// There is deliberately no minor version / compat negotiation: GUI and
    /// agent ship in one bundle and the agent re-execs itself when its binary
    /// is replaced, so strict equality plus a clean refusal is the whole
    /// contract (see [`PROTOCOL_VERSION`]).
    async fn protocol_version() -> u32;
    /// Accessibility / hook / autostart state for the GUI gate + settings.
    async fn status() -> AgentStatus;
    /// Latest device inventory snapshot (the GUI polls this on a timer while a
    /// window is open).
    async fn inventory() -> Vec<DeviceInventory>;
    /// Re-read `config.toml` and rebuild the live binding/DPI maps. Called by
    /// the GUI after it saves a config change.
    async fn reload_config() -> Result<(), ConfigReloadError>;
    /// Apply a DPI value to `route` now (slider preview / commit).
    async fn set_dpi(route: DeviceRoute, dpi: Dpi) -> Result<(), WriteError>;
    /// Apply a lighting config to `route` now.
    async fn set_lighting(route: DeviceRoute, lighting: Lighting) -> Result<(), WriteError>;
    /// Apply a full SmartShift config to `route` now.
    async fn set_smartshift(route: DeviceRoute, status: SmartShiftStatus)
    -> Result<(), WriteError>;
    /// Read the current DPI + supported values from `route`. A permanent error
    /// (`FeatureUnsupported` / `EmptyDpiList`) reaches the GUI intact so it can
    /// stop re-probing a device that genuinely lacks the feature.
    async fn read_dpi(route: DeviceRoute) -> Result<DpiInfo, WriteError>;
    /// Read the current SmartShift config from `route`.
    async fn read_smartshift(route: DeviceRoute) -> Result<SmartShiftStatus, WriteError>;
    /// Prompt for Accessibility from the agent, so the system dialog names the
    /// agent — the actually-trusted binary — rather than the GUI.
    async fn request_accessibility_prompt();
    /// Begin a pairing session against `selector`. The agent owns all device
    /// I/O, so pairing (which opens the receiver) runs here, not in the GUI —
    /// the GUI opening a receiver channel would clash with the agent's live
    /// capture session on the same Bolt receiver.
    async fn start_pairing(selector: ReceiverSelector) -> Result<(), PairingCommandError>;
    /// Bolt: pair with a discovered device by its address (from a prior
    /// [`PairingUpdate::DeviceFound`]).
    async fn pair_device(address: [u8; 6]) -> Result<(), PairingCommandError>;
    /// Abort the in-progress pairing session.
    async fn cancel_pairing() -> Result<(), PairingCommandError>;
    /// Long-poll the next pairing step. Returns `None` when the agent's hold
    /// window elapses with no event.
    ///
    /// Superseded by [`PairingPhase`] in [`AgentSnapshot::pairing`], which the
    /// GUI observes instead — a session's progress is state, so it survives a
    /// missed poll and a reconnect. The agent still serves this faithfully; the
    /// method stays because trait order is wire format.
    async fn next_pairing() -> Option<PairingUpdate>;
    /// Atomically fetch status and the latest inventory for the GUI poll loop.
    ///
    /// Appended for protocol v4. Keep future methods append-only; method order
    /// is wire-sensitive (see [`Self::protocol_version`]).
    async fn snapshot() -> AgentSnapshot;
    /// Drain the events the hook has observed since the last poll, for the GUI's
    /// live event monitor. The first poll enables monitoring; the agent
    /// auto-disables it once polls stop (the GUI closed the panel or died), so
    /// there is no explicit stop. Its position is retained before the Actions
    /// Ring methods appended by protocol v13/v14.
    async fn poll_event_monitor() -> Vec<MonitorEvent>;
    /// Apply a semantic standalone-light command to a raw HID route.
    async fn set_light(route: DeviceRoute, command: LightCommand) -> Result<(), WriteError>;
    /// Manually override effective power for a camera-linked light until the
    /// next aggregate camera-use transition.
    async fn set_light_manual_power(route: DeviceRoute, enabled: bool) -> Result<(), WriteError>;
    /// Long-poll the next Actions Ring invocation.
    ///
    /// Superseded by [`Agent::observe_action_ring`], which reports the showing
    /// ring as state: an overlay that restarts mid-ring then paints the live one
    /// instead of having missed its invocation. The method stays because trait
    /// order is wire format.
    async fn next_action_ring() -> Option<ActionRingInvocation>;
    /// Report a changed highlighted slot for optional haptic feedback.
    async fn action_ring_hover(
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError>;
    /// Execute the snapshotted action in `slot` and consume the session.
    async fn action_ring_activate(
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError>;
    /// Close a ring without executing an action.
    async fn action_ring_cancel(session_id: u64);
    /// Identity of this agent *run* — the Actions Ring overlay latches it and
    /// exits when it changes, so a helper orphaned by an agent restart yields
    /// the `overlay.lock` role instead of wedging its replacement out of it
    /// (#621, #644).
    ///
    /// **Frozen.** Both halves of [`Identity`] are plain `u64`s and this
    /// method's position never changes, so any past or future build can read
    /// it. New facts about the agent go on a new method or in [`AgentStatus`],
    /// never in here — an identity a helper cannot decode is an identity that
    /// cannot tell it to leave.
    async fn identity() -> Identity;
    /// Block until the agent's observable state differs from `since`, then
    /// return it whole.
    ///
    /// This is the state channel the GUI runs on. The *timing* is edge-driven —
    /// the agent answers the moment one of its watchers changes something — but
    /// the *content* is always the complete current state, never a delta, so a
    /// client converges from any starting point (a fresh window, a reconnect, an
    /// agent restart) by asking once with the last generation it saw, or `0`.
    /// That is what keeps the protocol free of subscriptions, replay, and
    /// gap detection: nothing has to be remembered on either side.
    ///
    /// With nothing to report the agent answers after [`OBSERVE_HOLD`] with the
    /// unchanged state, which doubles as a liveness heartbeat — an agent that is
    /// hung rather than gone stops answering, while a dead one drops the socket.
    async fn observe(since: Generation) -> Observation;
    /// Block until the ring the overlay should be showing differs from `since`,
    /// then return it. Same contract as [`Agent::observe`] — whole state, hold
    /// window, `0` for "seen nothing" — over the ring's own cell.
    async fn observe_action_ring(since: Generation) -> RingObservation;
    /// Declare what kind of client this connection is. Informational for an
    /// armed agent, load-bearing for a dormant one: the macOS dormancy gate
    /// arms only on [`ClientKind::Gui`]. The takeover probe never declares —
    /// it speaks only [`Agent::protocol_version`] — and so never arms.
    async fn declare_client(kind: ClientKind);
}

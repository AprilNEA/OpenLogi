//! tarpc service contract between the GUI (client) and the agent (server).
//!
//! tarpc generates the `AgentClient` and the `serve` glue from this trait. tarpc
//! is strict request/response — no server push — so the streaming needs become
//! polling: the GUI polls [`Agent::snapshot`] on a timer, and the Add Device
//! flow long-polls [`Agent::next_pairing`], which the agent holds open until a
//! pairing event arrives or the request deadline elapses.

use openlogi_core::config::Lighting;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiInfo, OnboardProfilesInfo, PairingError, PasskeyMethod, ProfilesMode,
    ReceiverSelector, SmartShiftMode, SmartShiftStatus, WriteError,
};
use serde::{Deserialize, Serialize};

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
/// v11: `set_onboard_profiles` / `read_onboard_profiles` appended,
///      `Capabilities::onboard_profiles` appended, `HidppOperation` gains
///      `ReadOnboardProfiles` / `WriteOnboardProfiles`.
pub const PROTOCOL_VERSION: u32 = 11;

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
pub struct AgentStatus {
    pub accessibility_granted: bool,
    pub hook_installed: bool,
    pub launch_at_login: bool,
    /// See [`InventoryHealth`]; the GUI picks its empty-state body off this.
    pub inventory: InventoryHealth,
    pub protocol_version: u32,
    pub agent_version: String,
}

/// Status and inventory as one poll result. Kept together so the GUI never
/// pairs inventory readiness from one orchestrator state with the inventory
/// list from another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub status: AgentStatus,
    pub inventory: Vec<DeviceInventory>,
}

/// A nearby unpaired device surfaced during Bolt discovery, in the minimal form
/// the GUI needs: a name to show and the address to pair by. The agent keeps the
/// full [`openlogi_hid::DiscoveredDevice`] (kind, auth bits) internally, keyed by
/// this address, so the wire form needs neither the non-serializable device-kind
/// nor the auth bitfield.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    async fn reload_config();
    /// Apply a DPI value to `route` now (slider preview / commit).
    async fn set_dpi(route: DeviceRoute, dpi: u32) -> Result<(), WriteError>;
    /// Apply a lighting config to `route` now.
    async fn set_lighting(route: DeviceRoute, lighting: Lighting) -> Result<(), WriteError>;
    /// Apply a full SmartShift config to `route` now.
    async fn set_smartshift(
        route: DeviceRoute,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) -> Result<(), WriteError>;
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
    /// window elapses with no event (the GUI simply re-polls); the GUI drives
    /// this in a loop while the Add Device window is open.
    async fn next_pairing() -> Option<PairingUpdate>;
    /// Atomically fetch status and the latest inventory for the GUI poll loop.
    ///
    /// Appended for protocol v4. Keep future methods append-only; method order
    /// is wire-sensitive (see [`Self::protocol_version`]).
    async fn snapshot() -> AgentSnapshot;
    /// Drain the events the hook has observed since the last poll, for the GUI's
    /// live event monitor. The first poll enables monitoring; the agent
    /// auto-disables it once polls stop (the GUI closed the panel or died), so
    /// there is no explicit stop.
    async fn poll_event_monitor() -> Vec<MonitorEvent>;
    /// Apply an onboard-profiles config to `route` now: host mode (OpenLogi
    /// drives the device) or onboard mode with an optional profile sector to
    /// activate. Appended for protocol v11.
    async fn set_onboard_profiles(
        route: DeviceRoute,
        mode: ProfilesMode,
        profile: Option<u16>,
    ) -> Result<(), WriteError>;
    /// Read the onboard-profiles state (description, mode, active profile,
    /// directory) from `route`. Appended last — see the method-order note on
    /// [`Agent::protocol_version`].
    async fn read_onboard_profiles(route: DeviceRoute) -> Result<OnboardProfilesInfo, WriteError>;
}

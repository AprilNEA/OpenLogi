//! tarpc service contract between the GUI (client) and the agent (server).
//!
//! tarpc generates the `AgentClient` and the `serve` glue from this trait. tarpc
//! is strict request/response — no server push — so the streaming needs become
//! polling: the GUI polls [`Agent::inventory`]/[`Agent::status`] on a timer, and
//! the Add Device flow long-polls [`Agent::next_pairing`], which the agent holds
//! open until a pairing event arrives or the request deadline elapses.

use openlogi_core::config::Lighting;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiInfo, PasskeyMethod, ReceiverSelector, SmartShiftMode, SmartShiftStatus,
    WriteError,
};
use serde::{Deserialize, Serialize};

/// Wire-protocol version. Bumped only on a breaking change to the types below —
/// independent of the crate version. The GUI checks it via
/// [`Agent::protocol_version`] on connect and refuses to drive a mismatch
/// (transient only: both binaries ship in one `.app` and update atomically).
pub const PROTOCOL_VERSION: u32 = 1;

/// Agent health the GUI surfaces: the Accessibility gate, whether the hook is
/// live, and the autostart toggle state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub accessibility_granted: bool,
    pub hook_installed: bool,
    pub launch_at_login: bool,
    pub protocol_version: u32,
    pub agent_version: String,
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

/// One step of a pairing session, streamed to the GUI via [`Agent::next_pairing`].
/// Mirrors `openlogi_hid::PairingEvent` but in a wire-safe form — the discovered
/// device collapses to [`FoundDevice`] and the terminal error to a string.
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
    /// The flow ended without pairing a device (carries a human-readable detail).
    Failed(String),
}

#[tarpc::service]
pub trait Agent {
    /// Wire-protocol version, for the connect handshake.
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
    async fn start_pairing(selector: ReceiverSelector);
    /// Bolt: pair with a discovered device by its address (from a prior
    /// [`PairingUpdate::DeviceFound`]).
    async fn pair_device(address: [u8; 6]);
    /// Abort the in-progress pairing session.
    async fn cancel_pairing();
    /// Long-poll the next pairing step. Returns `None` when the agent's hold
    /// window elapses with no event (the GUI simply re-polls); the GUI drives
    /// this in a loop while the Add Device window is open.
    async fn next_pairing() -> Option<PairingUpdate>;
}

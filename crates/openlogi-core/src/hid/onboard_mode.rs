//! Who controls the settings a gaming mouse's onboard profiles store.

use serde::{Deserialize, Serialize};

/// Whether a device's stored onboard profile (HID++ `0x8100`) or the host
/// controls its DPI, report rate and buttons. A device in onboard mode
/// rejects host DPI writes; the PRO X3 SUPERSTRIKE falls back to it on every
/// power cycle.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*
/// (Onboard=0, Host=1), so variant order is wire format and changes require
/// a `PROTOCOL_VERSION` bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
/// Persisted in `config.toml` as `onboard_mode = "onboard" | "host"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardMode {
    /// The active onboard profile applies; host writes to the settings it
    /// stores are rejected.
    Onboard,
    /// The host controls those settings; onboard profiles are inactive.
    Host,
}

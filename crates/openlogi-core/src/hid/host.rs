//! ChangeHost (`0x1814`) state shared between the agent and the GUI.

use serde::{Deserialize, Serialize};

/// Which host slot a multi-host device is on right now, and how many RF
/// channels it has. Labels the Flow tab's "This computer" card.
///
/// `host_count` counts the device's channels, not its pairings — an empty
/// slot still counts (see the host-switch session's `HostsInfo` handling).
///
/// Crosses the agent↔GUI IPC (`read_host_info`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Zero-based slot the device is currently paired-active on.
    pub current_host: u8,
    /// Number of host slots the device's radio supports (typically 3).
    pub host_count: u8,
}

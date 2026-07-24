//! HID++ `OnboardProfiles` (feature `0x8100`) — gaming-device profile memory.
//!
//! The protocol-level `0x8100` wrapper lives in `openlogi-hidpp`; this module
//! keeps OpenLogi's IPC-facing mode and snapshot types. In onboard mode the
//! device applies a profile from its own flash and ignores most host software
//! settings; OpenLogi therefore defaults such devices to host mode so the
//! configured DPI / buttons / report rate actually apply.

use serde::{Deserialize, Serialize};

/// Whether a gaming device applies its onboard flash profile or host software
/// settings.
///
/// Crosses the agent↔GUI IPC — serde encodes the variant *index* (Host=0,
/// Onboard=1), not a firmware byte — so variant order is wire format and
/// changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`). The firmware byte mapping
/// lives in `write::onboard_profiles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfilesMode {
    /// The host drives the device; onboard profiles are dormant.
    Host,
    /// The device applies the profile stored in its onboard memory.
    Onboard,
}

/// One entry of the device's onboard profile directory.
///
/// Crosses the agent↔GUI IPC (`read_onboard_profiles`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEntry {
    /// Flash sector holding the profile. User profiles live in sectors
    /// `0x0001..`; ROM (factory) profiles carry the `0x0100` flag.
    pub sector: u16,
    /// Whether the profile is enabled on the device.
    pub enabled: bool,
}

impl ProfileEntry {
    /// Whether this is a ROM (factory) profile rather than a writable user
    /// profile.
    #[must_use]
    pub fn is_rom(&self) -> bool {
        self.sector & 0x0100 != 0
    }
}

/// Snapshot of a device's onboard-profiles state.
///
/// Crosses the agent↔GUI IPC (`read_onboard_profiles`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfilesInfo {
    /// Number of writable user profiles.
    pub profile_count: u8,
    /// Number of out-of-box (ROM) profiles.
    pub profile_count_oob: u8,
    /// Number of physical buttons covered by a profile.
    pub button_count: u8,
    /// Number of writable flash sectors.
    pub sector_count: u8,
    /// Size of one flash sector in bytes.
    pub sector_size: u16,
    /// Memory model identifier (raw, informational).
    pub memory_model_id: u8,
    /// Profile format identifier (raw, informational).
    pub profile_format_id: u8,
    /// Macro format identifier (raw, informational).
    pub macro_format_id: u8,
    /// Whether the device is in host or onboard mode.
    pub mode: ProfilesMode,
    /// Sector of the active profile; `0x0000` when none has been activated.
    pub active_profile: u16,
    /// The profile directory (enabled and disabled entries).
    pub directory: Vec<ProfileEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_flag_is_detected() {
        assert!(
            ProfileEntry {
                sector: 0x0101,
                enabled: true
            }
            .is_rom()
        );
        assert!(
            !ProfileEntry {
                sector: 0x0002,
                enabled: true
            }
            .is_rom()
        );
    }
}

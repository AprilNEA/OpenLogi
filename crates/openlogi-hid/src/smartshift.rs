//! HID++ `SmartShift Enhanced` (feature `0x2111`) — wheel ratchet ↔
//! free-spin control with sensitivity threshold.
//!
//! The protocol-level `0x2111` wrapper lives in `openlogi-hidpp`; this module
//! keeps OpenLogi's IPC/config-facing mode and status types.
//!
//! Mode encoding (consistent across 0x2110 / 0x2111):
//! - `wheelMode` `1` = free-spin (no ratchet, infinite scroll), `2` =
//!   ratchet (clicky).
//! - `autoDisengage` `0x01`–`0xFE` = the wheel speed (in 0.25 turn/s steps)
//!   past which a ratchet-mode wheel releases into free-spin — i.e. the
//!   "SmartShift" threshold. `0xFF` keeps the ratchet engaged permanently
//!   (never auto-switches). See [`AUTO_DISENGAGE_PERMANENT`].

use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};

/// SmartShift mode values understood by the firmware. `Free` = free-spin,
/// `Ratchet` = clicky / smartshift-off. The discriminant is the wire byte;
/// reserved values (`0` / `3` / future) fail [`TryFrom`] and callers fall back
/// to whatever they consider sane.
///
/// Also crosses the agent↔GUI IPC — where serde encodes the variant *index*
/// (Free=0, Ratchet=1), not the `#[repr(u8)]` firmware discriminant — so
/// variant order is wire format and changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive, Serialize, Deserialize,
)]
#[repr(u8)]
pub enum SmartShiftMode {
    /// Wheel is in free-spin mode.
    Free = 1,
    /// Wheel is in ratchet mode.
    Ratchet = 2,
}

impl SmartShiftMode {
    /// The opposite mode — used by [`crate::write::toggle_smartshift`].
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Free => Self::Ratchet,
            Self::Ratchet => Self::Free,
        }
    }
}

// The config file persists the wheel mode in its own representation
// (`openlogi_core::config::WheelMode`, kept IPC-free); these conversions are
// the single mapping between the persisted and the wire/firmware form, used by
// the GUI when committing and by the agent when re-applying after a reconnect.
impl From<openlogi_core::config::WheelMode> for SmartShiftMode {
    fn from(mode: openlogi_core::config::WheelMode) -> Self {
        match mode {
            openlogi_core::config::WheelMode::Free => Self::Free,
            openlogi_core::config::WheelMode::Ratchet => Self::Ratchet,
        }
    }
}

impl From<SmartShiftMode> for openlogi_core::config::WheelMode {
    fn from(mode: SmartShiftMode) -> Self {
        match mode {
            SmartShiftMode::Free => Self::Free,
            SmartShiftMode::Ratchet => Self::Ratchet,
        }
    }
}

/// `autoDisengage` value that keeps the ratchet engaged permanently — the
/// wheel never auto-releases into free-spin, regardless of speed. Any other
/// value (`0x01`–`0xFE`) is a SmartShift speed threshold.
pub const AUTO_DISENGAGE_PERMANENT: u8 = 0xff;

/// Snapshot returned from OpenLogi's SmartShift read helpers.
///
/// Crosses the agent↔GUI IPC (`read_smartshift`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartShiftStatus {
    /// Current wheel mode.
    pub mode: SmartShiftMode,
    /// SmartShift speed threshold: `0x01`–`0xFE` in 0.25 turn/s steps (higher
    /// = harder to flip into free-spin while scrolling; Logitech defaults to
    /// ~16 on the MX line), or [`AUTO_DISENGAGE_PERMANENT`] for a permanently
    /// engaged ratchet.
    pub auto_disengage: u8,
    /// Tunable-torque force as a percentage (`1`–`100`) of the device's max
    /// force, or `0` when the device doesn't support tunable torque. Read back
    /// and re-sent unchanged so adjusting the mode or threshold doesn't
    /// disturb the wheel's resistance.
    pub tunable_torque: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipped_is_an_involution() {
        assert_eq!(SmartShiftMode::Free.flipped(), SmartShiftMode::Ratchet);
        assert_eq!(SmartShiftMode::Ratchet.flipped(), SmartShiftMode::Free);
        assert_eq!(
            SmartShiftMode::Free.flipped().flipped(),
            SmartShiftMode::Free
        );
    }
}

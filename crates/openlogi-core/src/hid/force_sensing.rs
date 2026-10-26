//! Press-force read-back snapshot of a force-sensing button — pure data, no
//! I/O.
//!
//! The HID++ `0x19c0` reads and writes that produce a [`ForceSensingInfo`]
//! live in `openlogi_device::write::force_sensing`.

use serde::{Deserialize, Serialize};

/// The press force a force-sensing button needs before it clicks, such as
/// the MX Master 4's Haptic Sense Panel, and the range the device accepts.
///
/// Forces are raw device values with no physical unit: a lower value is a
/// lighter press. The range differs from unit to unit, so it is always the
/// one the device reported, never a fixed one.
///
/// Crosses the agent↔GUI IPC, so field order is wire format — changes require
/// a `PROTOCOL_VERSION` bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForceSensingInfo {
    /// Whether the device lets software change the force. When `false` the
    /// device layer refuses every write.
    pub changeable: bool,
    /// The force the button starts at out of the box.
    pub default: u16,
    /// The lightest force the device accepts.
    pub min: u16,
    /// The heaviest force the device accepts.
    pub max: u16,
    /// The force the button is set to now.
    pub current: u16,
}

impl ForceSensingInfo {
    /// The force nearest to `force` inside the device's range: a saved value
    /// from another unit, or a hand-edited one, lands on the nearer end.
    #[must_use]
    pub fn clamp(&self, force: u16) -> u16 {
        // The device layer never builds an inverted range; ordering the ends
        // keeps a hand-built one from panicking.
        force.clamp(self.min.min(self.max), self.max.max(self.min))
    }
}

#[cfg(test)]
mod tests {
    use super::ForceSensingInfo;

    const MX_MASTER_4: ForceSensingInfo = ForceSensingInfo {
        changeable: true,
        default: 5089,
        min: 4071,
        max: 6768,
        current: 5089,
    };

    #[test]
    fn a_force_inside_the_range_is_kept() {
        assert_eq!(MX_MASTER_4.clamp(4071), 4071);
        assert_eq!(MX_MASTER_4.clamp(5500), 5500);
        assert_eq!(MX_MASTER_4.clamp(6768), 6768);
    }

    #[test]
    fn a_force_outside_the_range_lands_on_the_nearer_end() {
        assert_eq!(MX_MASTER_4.clamp(0), 4071);
        assert_eq!(MX_MASTER_4.clamp(u16::MAX), 6768);
    }

    #[test]
    fn an_inverted_range_still_clamps() {
        let inverted = ForceSensingInfo {
            min: 6768,
            max: 4071,
            ..MX_MASTER_4
        };
        assert_eq!(inverted.clamp(0), 4071);
        assert_eq!(inverted.clamp(u16::MAX), 6768);
    }
}

//! Wire-safe state and guarded replacement semantics for HID++ `0x4521`.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

use serde::{Deserialize, Serialize};

use super::{HidppOperation, WriteError};

/// Raw HID++ `DisableKeys` mask.
///
/// Unknown bits are retained so callers can preserve device-advertised keys
/// introduced by newer firmware without treating them as UI-editable keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisableKeysMask(u8);

impl DisableKeysMask {
    /// Empty mask.
    pub const EMPTY: Self = Self(0);
    /// Caps Lock key.
    pub const CAPS_LOCK: Self = Self(1 << 0);
    /// Num Lock key.
    pub const NUM_LOCK: Self = Self(1 << 1);
    /// Scroll Lock key.
    pub const SCROLL_LOCK: Self = Self(1 << 2);
    /// Insert key.
    pub const INSERT: Self = Self(1 << 3);
    /// Windows/Command key.
    pub const WINDOWS_COMMAND: Self = Self(1 << 4);
    /// Every key understood by this OpenLogi version.
    pub const KNOWN: Self = Self(0x1f);

    /// Construct a mask without discarding unknown bits.
    #[must_use]
    pub const fn from_bits_retain(bits: u8) -> Self {
        Self(bits)
    }

    /// Return the raw HID++ byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the mask has no asserted bits.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every bit in `other` is asserted in this mask.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitAnd for DisableKeysMask {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for DisableKeysMask {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitOr for DisableKeysMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DisableKeysMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Not for DisableKeysMask {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

/// Capability and current-state snapshot for HID++ `DisableKeys`.
///
/// Field order is IPC wire format and must remain append-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisableKeysState {
    /// Raw mask of keys the firmware advertises as disableable.
    pub supported: DisableKeysMask,
    /// Raw mask currently reported as disabled.
    pub disabled: DisableKeysMask,
}

impl DisableKeysState {
    /// Validate a requested known-key mask and compute the exact replacement.
    ///
    /// Advertised unknown disabled bits are preserved. Requested unknown bits,
    /// unsupported known bits, and unadvertised current bits are never written.
    pub fn replacement_for(
        self,
        requested: DisableKeysMask,
    ) -> Result<DisableKeysMask, WriteError> {
        let requested_unknown = requested & !DisableKeysMask::KNOWN;
        let requested_unsupported = requested & DisableKeysMask::KNOWN & !self.supported;
        if !requested_unknown.is_empty() || !requested_unsupported.is_empty() {
            return Err(WriteError::UnsupportedMask {
                operation: HidppOperation::WriteDisableKeys,
                feature_hex: 0x4521,
                requested: u64::from(requested.bits()),
                supported: u64::from(self.supported.bits()),
            });
        }

        Ok((requested & DisableKeysMask::KNOWN)
            | (self.disabled & self.supported & !DisableKeysMask::KNOWN))
    }
}

#[cfg(test)]
mod tests {
    use super::{DisableKeysMask, DisableKeysState};

    #[test]
    fn raw_mask_retains_known_and_unknown_bits() {
        assert_eq!(DisableKeysMask::CAPS_LOCK.bits(), 0x01);
        assert_eq!(DisableKeysMask::WINDOWS_COMMAND.bits(), 0x10);
        assert_eq!(DisableKeysMask::KNOWN.bits(), 0x1f);
        assert_eq!(DisableKeysMask::from_bits_retain(0xa1).bits(), 0xa1);
    }

    #[test]
    fn replacement_is_capability_bounded() {
        let state = DisableKeysState {
            supported: DisableKeysMask::from_bits_retain(0xa1),
            disabled: DisableKeysMask::from_bits_retain(0xe0),
        };

        assert_eq!(
            state
                .replacement_for(DisableKeysMask::CAPS_LOCK)
                .expect("advertised known key is valid"),
            DisableKeysMask::from_bits_retain(0xa1)
        );
    }
}

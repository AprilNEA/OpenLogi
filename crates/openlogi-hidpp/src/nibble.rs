//! A very simple u4/nibble implementation.

/// Represents an unsigned 4-bit value (nibble) encoded as a byte.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct U4(u8);

impl U4 {
    /// Constructs a nibble from the 4 low/rightmost bits of a byte.
    pub fn from_lo(raw: u8) -> Self {
        Self(raw & 0x0f)
    }

    /// Constructs a nibble from the 4 high/leftmost bits of a byte.
    pub fn from_hi(raw: u8) -> Self {
        Self(raw >> 4)
    }

    /// Constructs a byte with the nibble set as the 4 low/rightmost bits.
    pub fn to_lo(self) -> u8 {
        self.0
    }

    /// Constructs a byte with the nibble set as the 4 high/leftmost bits.
    pub fn to_hi(self) -> u8 {
        self.0 << 4
    }
}

/// Combines two nibbles to a byte, with `a` being set to the 4 leftmost and
/// `b` being set to the 4 rightmost bits.
pub fn combine(a: U4, b: U4) -> u8 {
    a.to_hi() | b.to_lo()
}

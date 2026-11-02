//! Implements the `LockKeyState` feature (ID `0x4220`) that reports which of a
//! keyboard's lock keys are engaged: Num Lock, Caps Lock and Scroll Lock.
//!
//! No public Logitech spec covers this feature. The layout here is
//! reverse-engineered from Logi Options+ 2.8 (`devio::Feature4220LockKeyState`
//! and the `logi::devices::devio_feature_4220` handler):
//!
//! - `getLockState` (function `0`) takes no parameters and answers with the
//!   lock bitmask in byte `0`.
//! - The keyboard broadcasts the same byte when a lock key changes. Options+
//!   reads byte `0` of any unsolicited report on this feature without checking
//!   its function, and [`LockKeyStateFeature::decode_broadcast`] does the same.
//! - The bit order follows the Options+ handler, which maps bit `0` to Num
//!   Lock, bit `1` to Caps Lock and bit `2` to Scroll Lock. That is also the
//!   order of the HID keyboard LED usages (Num Lock `1`, Caps Lock `2`, Scroll
//!   Lock `3`).

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

#[cfg(test)]
mod tests;

/// Implements the `LockKeyState` / `0x4220` feature.
#[derive(Feature)]
#[creatable(id = 0x4220, version = 0)]
pub struct LockKeyStateFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl LockKeyStateFeature {
    /// Reads which lock keys are engaged (function `0`, `getLockState`).
    pub async fn get_lock_state(&self) -> Result<LockKeys, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(Self::decode_broadcast(&payload))
    }

    /// Decodes the lock keys from an unsolicited report on this feature's
    /// index.
    ///
    /// Infallible on purpose: every bit pattern is a valid lock state, and a
    /// bit this crate does not name is kept rather than dropped.
    #[must_use]
    pub const fn decode_broadcast(payload: &[u8; 16]) -> LockKeys {
        LockKeys::from_bits_retain(payload[0])
    }
}

bitflags::bitflags! {
    /// The lock keys a keyboard reports as engaged.
    ///
    /// Unnamed bits are legal and kept, so a newer keyboard's extra lock key
    /// never makes the reading fail.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct LockKeys: u8 {
        /// Num Lock is engaged.
        const NUM_LOCK = 1 << 0;
        /// Caps Lock is engaged.
        const CAPS_LOCK = 1 << 1;
        /// Scroll Lock is engaged.
        const SCROLL_LOCK = 1 << 2;
    }
}

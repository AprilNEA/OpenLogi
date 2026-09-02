//! Implements `GamingMKeys` (feature `0x8020`).
//!
//! Logitech's public HID++ index names this feature but does not publish its
//! function table. The count response and state-event payload used here were
//! verified on a G913 and remain explicitly reverse-engineered protocol facts.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Currently-held gaming mode keys from a `0x8020` state event.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct MKeyState: u8 {
        /// M1 is held.
        const M1 = 1 << 0;
        /// M2 is held.
        const M2 = 1 << 1;
        /// M3 is held.
        const M3 = 1 << 2;
    }
}

/// An event emitted by [`GamingMKeysFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum GamingMKeysEvent {
    /// Full snapshot of the mode keys currently held.
    StateChanged(MKeyState),
}

impl DecodeEvent for GamingMKeysEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then_some(Self::StateChanged(MKeyState::from_bits_retain(payload[0])))
    }
}

/// Implements the `GamingMKeys` / `0x8020` feature.
#[derive(Feature)]
#[creatable(id = 0x8020, version = 0)]
pub struct GamingMKeysFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded mode-key state snapshots.
    events: EventSource<GamingMKeysEvent>,
}

impl GamingMKeysFeature {
    /// Returns the number of physical M-keys.
    ///
    /// This is reverse-engineered function 0. G913 reports `3` in the first
    /// response byte.
    pub async fn key_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }
}

#[cfg(test)]
mod tests {
    use super::{GamingMKeysEvent, MKeyState};
    use crate::feature::DecodeEvent;

    #[test]
    fn decodes_full_m_key_state_snapshot() {
        let mut payload = [0; 16];
        payload[0] = (MKeyState::M1 | MKeyState::M3).bits();

        assert_eq!(
            GamingMKeysEvent::decode(0, &payload),
            Some(GamingMKeysEvent::StateChanged(
                MKeyState::M1 | MKeyState::M3
            ))
        );
        assert_eq!(GamingMKeysEvent::decode(1, &payload), None);
    }
}

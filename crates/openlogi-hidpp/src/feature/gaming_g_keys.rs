//! Implements `GamingGKeys` (feature `0x8010`).
//!
//! Logitech's public HID++ index names this feature but does not publish its
//! function table. The software-control function and state-event payload used
//! here are reverse-engineered protocol facts and must be checked on hardware.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Currently-held gaming G-keys from a `0x8010` state event.
    ///
    /// Unknown bits are retained so keyboards with more than eight G-keys do
    /// not silently turn a partially-understood state into a false release.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct GKeyState: u8 {
        /// G1 is held.
        const G1 = 1 << 0;
        /// G2 is held.
        const G2 = 1 << 1;
        /// G3 is held.
        const G3 = 1 << 2;
        /// G4 is held.
        const G4 = 1 << 3;
        /// G5 is held.
        const G5 = 1 << 4;
        /// G6 is held.
        const G6 = 1 << 5;
        /// G7 is held.
        const G7 = 1 << 6;
        /// G8 is held.
        const G8 = 1 << 7;
    }
}

/// An event emitted by [`GamingGKeysFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum GamingGKeysEvent {
    /// Full snapshot of the G-keys currently held.
    StateChanged(GKeyState),
}

impl DecodeEvent for GamingGKeysEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then_some(Self::StateChanged(GKeyState::from_bits_retain(payload[0])))
    }
}

/// Implements the `GamingGKeys` / `0x8010` feature.
#[derive(Feature)]
#[creatable(id = 0x8010, version = 0)]
pub struct GamingGKeysFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded G-key state snapshots.
    events: EventSource<GamingGKeysEvent>,
}

impl GamingGKeysFeature {
    /// Selects host software event reporting (`true`) or onboard handling
    /// (`false`) for the G-key row.
    ///
    /// This is reverse-engineered function 2. Enabling it must be repeated
    /// after the keyboard reconnects or re-enumerates.
    pub async fn set_software_control(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [u8::from(enabled), 0, 0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{GKeyState, GamingGKeysEvent};
    use crate::feature::DecodeEvent;

    #[test]
    fn decodes_full_g_key_state_snapshot() {
        let mut payload = [0; 16];
        payload[0] = (GKeyState::G1 | GKeyState::G5).bits();

        assert_eq!(
            GamingGKeysEvent::decode(0, &payload),
            Some(GamingGKeysEvent::StateChanged(
                GKeyState::G1 | GKeyState::G5
            ))
        );
        assert_eq!(GamingGKeysEvent::decode(1, &payload), None);
    }
}

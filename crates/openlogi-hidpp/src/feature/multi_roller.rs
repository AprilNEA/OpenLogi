//! Implements the `MultiRoller` feature (ID `0x4610`): the rotary encoders of
//! the MX Creative Dialpad — its large dial and its small roller.
//!
//! No public Logitech spec exists for this feature. The function and field
//! *names* come from the protobuf descriptor `logi/protocol/multiroller.proto`
//! shipped inside Logi Options+; every byte offset below was
//! **reverse-engineered on hardware** (`docs/hardware/mx-creative-console.md`,
//! dialpad firmware `RBO07.00_B0016`, feature version 1). Functions `4..` answer
//! "invalid function id", so direction and speed are host-side settings.

#[cfg(test)]
mod tests;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements the `MultiRoller` / `0x4610` feature.
#[derive(Feature)]
#[creatable(id = 0x4610, version = 1)]
pub struct MultiRollerFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<MultiRollerEvent>,
}

/// Identifies one roller of a [`MultiRollerFeature`] device, `0..roller_count`.
///
/// On the MX Creative Dialpad `0` is the small roller and `1` the large dial
/// (hardware-observed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RollerId(pub u8);

/// Device-wide roller information, from
/// [`get_capabilities`](MultiRollerFeature::get_capabilities).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct MultiRollerCapabilities {
    /// Number of rollers; valid [`RollerId`]s are `0..roller_count`.
    pub roller_count: u8,
}

impl MultiRollerCapabilities {
    /// Every roller id this device answers for.
    pub fn rollers(self) -> impl Iterator<Item = RollerId> {
        (0..self.roller_count).map(RollerId)
    }
}

/// Per-roller information, from
/// [`get_roller_capabilities`](MultiRollerFeature::get_roller_capabilities).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RollerCapabilities {
    /// Rotation increments per full revolution (dial `180`, roller `40` on the
    /// MX Creative Dialpad). Reverse-engineered: byte 0 matched the observed
    /// detent density of each roller.
    pub increments_per_revolution: u8,
}

/// How a roller reports its rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RollerReportingMode {
    /// Rotation goes to the native HID channel.
    Native = 0,
    /// Rotation is diverted to HID++ as [`MultiRollerEvent::Rotation`].
    Diverted = 1,
}

impl MultiRollerFeature {
    /// Retrieves the number of rollers.
    pub async fn get_capabilities(&self) -> Result<MultiRollerCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(MultiRollerCapabilities {
            roller_count: payload[0],
        })
    }

    /// Retrieves one roller's resolution.
    pub async fn get_roller_capabilities(
        &self,
        roller: RollerId,
    ) -> Result<RollerCapabilities, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [roller.0, 0, 0])
            .await?
            .extend_payload();
        Ok(RollerCapabilities {
            increments_per_revolution: payload[0],
        })
    }

    /// Retrieves one roller's reporting mode.
    pub async fn get_mode(&self, roller: RollerId) -> Result<RollerReportingMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [roller.0, 0, 0])
            .await?
            .extend_payload();
        decode_mode(payload[0])
    }

    /// Sets one roller's reporting mode. Divert a roller
    /// ([`RollerReportingMode::Diverted`]) for its rotation to arrive as
    /// [`MultiRollerEvent::Rotation`].
    ///
    /// The response echoes `[roller, mode]` and is not decoded; read the mode
    /// back with [`get_mode`](Self::get_mode) when it is needed.
    ///
    /// The write finishes on the wire before the call returns, as the G-key
    /// row's ownership writes do: a hand-back can then never overtake a
    /// stalled diversion and leave the roller diverted with nobody
    /// listening. The caller must drive it to completion.
    pub async fn set_mode(
        &self,
        roller: RollerId,
        mode: RollerReportingMode,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call_write_through(3, [roller.0, mode.into(), 0])
            .await?;
        Ok(())
    }
}

fn decode_mode(byte: u8) -> Result<RollerReportingMode, Hidpp20Error> {
    RollerReportingMode::try_from(byte).map_err(|_| Hidpp20Error::UnsupportedResponse)
}

/// An event emitted by [`MultiRollerFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum MultiRollerEvent {
    /// A diverted roller turned.
    Rotation(RollerRotation),
}

/// Payload of [`MultiRollerEvent::Rotation`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RollerRotation {
    /// The roller that turned.
    pub roller: RollerId,
    /// Increments turned since the previous event; positive is clockwise
    /// (dial) or up (roller).
    pub delta: i8,
    /// The device's free-running millisecond clock at the event.
    pub timestamp_ms: u32,
}

impl DecodeEvent for MultiRollerEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // The rotation report is the only event and carries sub-id 0.
        (sub_id == 0).then(|| MultiRollerEvent::Rotation(RollerRotation::from_payload(payload)))
    }
}

impl RollerRotation {
    /// Decode the payload of a `0x4610` event with sub-id (function) `0`.
    ///
    /// Public so a host session that filters raw channel messages itself
    /// decodes through this one layout instead of re-deriving it.
    #[must_use]
    pub fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            roller: RollerId(payload[0]),
            delta: payload[1].cast_signed(),
            timestamp_ms: u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]),
        }
    }
}

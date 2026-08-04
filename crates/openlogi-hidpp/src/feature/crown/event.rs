//! The event emitted by the `Crown` feature (`0x4600`).

use num_enum::{FromPrimitive, IntoPrimitive};

/// Rotation phase reported in a [`CrownUpdate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RotationState {
    /// Not rotating (or not diverted).
    Inactive = 0,
    /// Rotation started.
    Start = 1,
    /// Rotation ongoing.
    Active = 2,
    /// Rotation stopped.
    Stop = 3,
    /// A state this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Proximity or touch activity phase reported in a [`CrownUpdate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ActivityState {
    /// Inactive.
    Inactive = 0,
    /// Started.
    Start = 1,
    /// Ongoing.
    Active = 2,
    /// Stopped.
    Stop = 3,
    /// A state this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Touch gesture reported in a [`CrownUpdate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum CrownGesture {
    /// No gesture.
    None = 0,
    /// Single tap.
    Tap = 1,
    /// Double tap.
    DoubleTap = 2,
    /// A gesture this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Crown button state reported in a [`CrownUpdate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ButtonState {
    /// Inactive (or not diverted).
    Inactive = 0,
    /// Press started.
    Press = 1,
    /// Short press active.
    ShortPressActive = 2,
    /// Long press reached the time threshold.
    LongPress = 3,
    /// Long press active.
    LongPressActive = 4,
    /// Released.
    Release = 5,
    /// A state this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// An event emitted by [`CrownFeature`](super::CrownFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum CrownEvent {
    /// The crown's rotation, proximity, touch, gesture or button state changed.
    ///
    /// Only reported while the crown is diverted (see
    /// [`set_mode`](super::CrownFeature::set_mode)).
    Update(CrownUpdate),
}

/// Payload of [`CrownEvent::Update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct CrownUpdate {
    /// Current rotation phase.
    pub rotation_state: RotationState,
    /// Slots rotated since the last event (`-127..=127`).
    pub relative_slot_rotation: i8,
    /// Ratchets rotated since the last event (`-127..=127`).
    pub relative_ratchet_rotation: i8,
    /// Proximity-sensor phase.
    pub proximity: ActivityState,
    /// Touch-sensor phase.
    pub touch: ActivityState,
    /// Touch gesture detected.
    pub gesture: CrownGesture,
    /// Button state.
    pub button: ButtonState,
    /// Crown speed in slots per second (signed).
    pub speed: i16,
}

/// Decodes the `0x4600` event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<CrownEvent> {
    match sub_id {
        0 => Some(CrownEvent::Update(CrownUpdate {
            rotation_state: RotationState::from(payload[0]),
            relative_slot_rotation: payload[1] as i8,
            relative_ratchet_rotation: payload[2] as i8,
            proximity: ActivityState::from(payload[3]),
            touch: ActivityState::from(payload[4]),
            gesture: CrownGesture::from(payload[5]),
            button: ButtonState::from(payload[6]),
            speed: i16::from_be_bytes([payload[14], payload[15]]),
        })),
        _ => None,
    }
}

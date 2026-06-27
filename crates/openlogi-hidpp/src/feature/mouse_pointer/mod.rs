//! Implements the `MousePointer` feature (ID `0x2200`) that reports a mouse's
//! basic optical-sensor properties and pointer-tuning hints.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// The pointer-acceleration ("ballistics") curve a device suggests, based on its
/// physical characteristics.
///
/// A host that provides multiple ballistics curves can pick a default from this
/// hint; a host without its own ballistics ignores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PointerAcceleration {
    /// No acceleration suggested.
    None = 0,
    /// A low acceleration curve.
    Low = 1,
    /// A medium acceleration curve.
    Medium = 2,
    /// A high acceleration curve.
    High = 3,
}

/// Mouse-pointer information returned by
/// [`MousePointerFeature::get_mouse_pointer_info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct MousePointerInfo {
    /// Typical sensor resolution on a standard surface, in 1-DPI steps.
    ///
    /// Real-world resolution may differ from this value by up to ±20% depending
    /// on the surface.
    pub sensor_resolution: u16,

    /// The acceleration curve the device suggests.
    pub pointer_acceleration: PointerAcceleration,

    /// Whether the device suggests using the OS-native ballistics.
    ///
    /// `false` means the host may override the OS ballistics if it can; `true`
    /// means the device suggests keeping the OS-native ballistics.
    pub suggest_os_ballistics: bool,

    /// Whether the device suggests offering vertical-orientation tuning.
    ///
    /// `true` for devices such as trackballs, where the host can let the user
    /// fine-tune X/Y movement relative to cursor movement.
    pub suggest_vertical_tuning: bool,
}

/// Implements the `MousePointer` / `0x2200` feature.
#[derive(Clone)]
pub struct MousePointerFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for MousePointerFeature {
    const ID: u16 = 0x2200;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for MousePointerFeature {}

impl MousePointerFeature {
    /// Retrieves the sensor resolution and pointer-tuning hints of the mouse.
    pub async fn get_mouse_pointer_info(&self) -> Result<MousePointerInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        MousePointerInfo::from_payload(payload)
    }
}

impl MousePointerInfo {
    /// Decodes a `getMousePointerInfo` response payload.
    fn from_payload(payload: [u8; 16]) -> Result<Self, Hidpp20Error> {
        let flags = payload[2];
        Ok(Self {
            sensor_resolution: u16::from_be_bytes([payload[0], payload[1]]),
            // Acceleration occupies the low two bits; all four values are valid
            // so this conversion cannot actually fail.
            pointer_acceleration: PointerAcceleration::try_from(flags & 0b11)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            suggest_os_ballistics: flags & (1 << 2) != 0,
            suggest_vertical_tuning: flags & (1 << 3) != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MousePointerInfo, PointerAcceleration};

    #[test]
    fn decodes_resolution_and_flags() {
        let mut payload = [0; 16];
        payload[0..2].copy_from_slice(&1600u16.to_be_bytes());
        // High acceleration (0b11) + suggest OS ballistics (bit 2).
        payload[2] = 0b0000_0111;

        let info = MousePointerInfo::from_payload(payload).unwrap();
        assert_eq!(info.sensor_resolution, 1600);
        assert_eq!(info.pointer_acceleration, PointerAcceleration::High);
        assert!(info.suggest_os_ballistics);
        assert!(!info.suggest_vertical_tuning);
    }

    #[test]
    fn decodes_trackball_vertical_tuning() {
        let mut payload = [0; 16];
        payload[0..2].copy_from_slice(&400u16.to_be_bytes());
        // Suggest vertical tuning (bit 3), acceleration none.
        payload[2] = 0b0000_1000;

        let info = MousePointerInfo::from_payload(payload).unwrap();
        assert_eq!(info.pointer_acceleration, PointerAcceleration::None);
        assert!(!info.suggest_os_ballistics);
        assert!(info.suggest_vertical_tuning);
    }
}

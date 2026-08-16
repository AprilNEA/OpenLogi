//! Implements the reverse-engineered `HapticFeedback` feature (`0x19b0`).
//!
//! The function and payload layouts are cross-checked against Solaar and an MX
//! Master 4. Logitech has not published this feature in the public HID++ spec,
//! so additions must be verified against hardware rather than guessed.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

bitflags::bitflags! {
    /// Waveforms the device reports as playable.
    ///
    /// Unknown bits are retained so newer firmware does not silently lose
    /// capability information.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SupportedWaveforms: u32 {
        /// A damp state-change pulse, used after activating a ring action.
        const DAMP_STATE_CHANGE = 1 << 1;
        /// A subtle collision pulse, used when the highlighted ring slot changes.
        const SUBTLE_COLLISION = 1 << 4;
    }
}

/// A haptic waveform ID accepted by `playWaveform`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum HapticWaveform {
    /// Confirmation pulse used when an action runs.
    DampStateChange = 1,
    /// Light boundary pulse used for hover transitions.
    SubtleCollision = 4,
}

/// Valid device haptic intensity (`0..=100`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HapticIntensity(u8);

impl HapticIntensity {
    /// Highest accepted intensity percentage.
    pub const MAX: u8 = 100;

    /// Validate an intensity percentage.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Return the percentage sent on the wire.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Device-wide haptic configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticConfiguration {
    /// Whether firmware haptic playback is enabled.
    pub enabled: bool,
    /// Current intensity percentage.
    pub intensity: HapticIntensity,
    /// Number of discrete levels advertised by the firmware.
    pub level_count: u8,
    /// Percentage step between discrete levels.
    pub level_step: u8,
}

/// Haptic capabilities reported by the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HapticCapabilities {
    /// Bytes whose meaning has not yet been verified.
    pub unknown_prefix: [u8; 4],
    /// Supported waveform mask.
    pub waveforms: SupportedWaveforms,
}

/// Implements `HapticFeedback` / `0x19b0`.
#[derive(Clone, Feature)]
#[creatable(id = 0x19b0, version = 0)]
pub struct HapticFeedbackFeature {
    endpoint: FeatureEndpoint,
}

impl HapticFeedbackFeature {
    /// Read the device's supported waveform mask.
    pub async fn get_capabilities(&self) -> Result<HapticCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(HapticCapabilities {
            unknown_prefix: payload[0..4]
                .try_into()
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            waveforms: SupportedWaveforms::from_bits_retain(u32::from_be_bytes(
                payload[4..8]
                    .try_into()
                    .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            )),
        })
    }

    /// Read whether haptics are enabled and their current intensity.
    pub async fn get_configuration(&self) -> Result<HapticConfiguration, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        let enabled = match payload[0] {
            0 => false,
            1 => true,
            _ => return Err(Hidpp20Error::UnsupportedResponse),
        };
        let Some(intensity) = HapticIntensity::new(payload[1]) else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(HapticConfiguration {
            enabled,
            intensity,
            level_step: payload[2] >> 4,
            level_count: payload[2] & 0x0f,
        })
    }

    /// Write the device-wide haptic enabled state and intensity.
    pub async fn set_configuration(
        &self,
        enabled: bool,
        intensity: HapticIntensity,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(2, [u8::from(enabled), intensity.get(), 0])
            .await?;
        Ok(())
    }

    /// Play one typed haptic waveform immediately.
    pub async fn play(&self, waveform: HapticWaveform) -> Result<(), Hidpp20Error> {
        self.endpoint.call(4, [waveform.into(), 0, 0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_rejects_values_above_one_hundred() {
        assert_eq!(
            HapticIntensity::new(100).map(HapticIntensity::get),
            Some(100)
        );
        assert_eq!(HapticIntensity::new(101), None);
    }

    #[test]
    fn waveform_mask_retains_unknown_bits() {
        let mask = SupportedWaveforms::from_bits_retain((1 << 4) | (1 << 31));
        assert!(mask.contains(SupportedWaveforms::SUBTLE_COLLISION));
        assert_eq!(mask.bits(), (1 << 4) | (1 << 31));
    }
}

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

bitflags::bitflags! {
    /// The first byte of the haptic configuration.
    ///
    /// Unknown bits are retained: a write built from a read hands every bit
    /// this crate does not name back to the firmware unchanged.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HapticConfigFlags: u8 {
        /// Firmware haptic playback is enabled.
        const ENABLED = 1 << 0;
        /// Battery-saving haptics. Logi Options+ sets this bit from its own
        /// power-save setting, independently of [`Self::ENABLED`].
        const POWER_SAVE = 1 << 1;
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
    /// Enable, power-save, and any unnamed bits, as the device reported them.
    pub flags: HapticConfigFlags,
    /// Current intensity percentage.
    pub intensity: HapticIntensity,
    /// Number of discrete levels advertised by the firmware.
    pub level_count: u8,
    /// Percentage step between discrete levels.
    pub level_step: u8,
}

impl HapticConfiguration {
    fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [flags, intensity, levels, ..] = *payload else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self {
            flags: HapticConfigFlags::from_bits_retain(flags),
            intensity: HapticIntensity::new(intensity).ok_or(Hidpp20Error::UnsupportedResponse)?,
            level_step: levels >> 4,
            level_count: levels & 0x0f,
        })
    }
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

    /// Read the haptic flags and current intensity.
    pub async fn get_configuration(&self) -> Result<HapticConfiguration, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        HapticConfiguration::from_payload(&payload)
    }

    /// Write the device-wide haptic flags and intensity.
    ///
    /// `flags` replaces the whole byte: edit the value from
    /// [`Self::get_configuration`] so power-save and unnamed bits survive.
    pub async fn set_configuration(
        &self,
        flags: HapticConfigFlags,
        intensity: HapticIntensity,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(2, [flags.bits(), intensity.get(), 0])
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

    #[test]
    fn configuration_keeps_power_save_and_unnamed_flag_bits() {
        // Power save without enable, as Options+ writes it, plus an unnamed bit.
        let config = HapticConfiguration::from_payload(&[0x82, 60, 0x34]).unwrap();
        assert!(!config.flags.contains(HapticConfigFlags::ENABLED));
        assert!(config.flags.contains(HapticConfigFlags::POWER_SAVE));
        assert_eq!(config.flags.bits(), 0x82);
        assert_eq!(
            (config.flags | HapticConfigFlags::ENABLED).bits(),
            0x83,
            "enabling must leave power save and the unnamed bit in place"
        );
        assert_eq!(config.intensity.get(), 60);
        assert_eq!((config.level_step, config.level_count), (3, 4));
    }

    #[test]
    fn configuration_rejects_out_of_range_intensity() {
        assert!(matches!(
            HapticConfiguration::from_payload(&[0x01, 101, 0]),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}

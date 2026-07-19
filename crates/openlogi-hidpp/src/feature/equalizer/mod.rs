//! Implements the `Equalizer` feature (ID `0x8310`, version 2) that configures
//! an audio device's equalizer (per-band gains) and microphone noise reduction.
//!
//! The device exposes a single EQ table of `band_count` frequency bands; each
//! band has a fixed frequency (Hz) and an adjustable signed gain (dB). All
//! frequencies are big-endian `u16`; gains are signed `i8`.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Maximum number of frequencies a single `getFrequencies` response carries.
const FREQUENCIES_PER_PAGE: u8 = 7;

bitflags::bitflags! {
    /// How a device stores its EQ values, from
    /// [`get_eq_info`](EqualizerFeature::get_eq_info).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct EqCapabilities: u8 {
        /// EQ values are stored as gains.
        const STORED_AS_GAINS = 1 << 0;
        /// EQ values are stored as coefficients.
        const STORED_AS_COEFFICIENTS = 1 << 1;
    }
}

/// Where [`get_frequency_gains`](EqualizerFeature::get_frequency_gains) reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum GainLocation {
    /// The custom EQ stored in EEPROM (the version-0 default).
    Eeprom = 0,
    /// The active EQ in RAM.
    Ram = 1,
}

/// How [`set_frequency_gains`](EqualizerFeature::set_frequency_gains) persists
/// the gains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum GainPersistence {
    /// Volatile: applied to RAM only.
    Volatile = 0,
    /// Applied to RAM and stored in EEPROM.
    VolatileAndNonVolatile = 1,
    /// Stored in EEPROM only.
    NonVolatileOnly = 2,
}

/// EQ table information from [`get_eq_info`](EqualizerFeature::get_eq_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct EqInfo {
    /// Number of frequency bands.
    pub band_count: u8,
    /// Gain range in dB; used as `±db_range` when `db_min`/`db_max` are both `0`.
    pub db_range: u8,
    /// How EQ values are stored.
    pub capabilities: EqCapabilities,
    /// Minimum gain in dB, or `0` to imply `-db_range`.
    pub db_min: i8,
    /// Maximum gain in dB, or `0` to imply `+db_range`.
    pub db_max: i8,
}

impl EqInfo {
    fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            band_count: payload[0],
            db_range: payload[1],
            capabilities: EqCapabilities::from_bits_retain(payload[2]),
            db_min: payload[3] as i8,
            db_max: payload[4] as i8,
        }
    }

    /// The effective `(min, max)` gain range in dB.
    ///
    /// Resolves the "both zero implies `±db_range`" rule into concrete bounds.
    #[must_use]
    pub fn effective_range(&self) -> (i8, i8) {
        if self.db_min == 0 && self.db_max == 0 {
            let range = i8::try_from(self.db_range).unwrap_or(i8::MAX);
            (-range, range)
        } else {
            (self.db_min, self.db_max)
        }
    }
}

/// Implements the `Equalizer` / `0x8310` feature.
#[derive(Clone)]
pub struct EqualizerFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for EqualizerFeature {
    const ID: u16 = 0x8310;
    const STARTING_VERSION: u8 = 2;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for EqualizerFeature {}

impl EqualizerFeature {
    /// Retrieves the EQ table's band count, gain range and storage capabilities.
    pub async fn get_eq_info(&self) -> Result<EqInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(EqInfo::from_payload(&payload))
    }

    /// Retrieves the frequency (Hz) of every band.
    ///
    /// `band_count` is the value from [`EqInfo::band_count`]; the device returns
    /// up to seven frequencies per response, so this pages through them until all
    /// `band_count` are collected.
    pub async fn get_frequencies(&self, band_count: u8) -> Result<Vec<u16>, Hidpp20Error> {
        let mut frequencies = Vec::with_capacity(usize::from(band_count));
        let mut index = 0u8;
        while index < band_count {
            let payload = self.endpoint.call(1, [index, 0, 0]).await?.extend_payload();
            // The response echoes the requested band index in byte 0.
            if payload[0] != index {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            let page = (band_count - index).min(FREQUENCIES_PER_PAGE);
            frequencies.extend(parse_frequency_page(&payload, page)?);
            index += page;
        }
        Ok(frequencies)
    }

    /// Retrieves the active gain (dB) of every band from `location`.
    ///
    /// `band_count` is the value from [`EqInfo::band_count`] (at most 15, the
    /// number of gains a single response carries).
    pub async fn get_frequency_gains(
        &self,
        location: GainLocation,
        band_count: u8,
    ) -> Result<Vec<i8>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [location.into(), 0, 0])
            .await?
            .extend_payload();
        parse_gains(&payload, 0, band_count)
    }

    /// Sets the per-band gains (dB) and returns the device's echo of them.
    ///
    /// `gains` holds one signed value per band (at most 15). The device rejects
    /// out-of-range gains.
    pub async fn set_frequency_gains(
        &self,
        persistence: GainPersistence,
        gains: &[i8],
    ) -> Result<Vec<i8>, Hidpp20Error> {
        let count = u8::try_from(gains.len()).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        let mut args = [0; 16];
        args[0] = persistence.into();
        // Gains follow the persistence byte; each is a signed value sent as a raw
        // byte.
        for (i, &gain) in gains.iter().enumerate() {
            let slot = 1 + i;
            if slot >= args.len() {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            args[slot] = gain as u8;
        }
        let payload = self.endpoint.call_long(3, args).await?.extend_payload();
        // The response echoes the request, so the gains start after the echoed
        // persistence byte.
        parse_gains(&payload, 1, count)
    }

    /// Retrieves whether hardware microphone noise reduction is enabled.
    pub async fn get_mic_noise_reduction(&self) -> Result<bool, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(payload[0] != 0)
    }

    /// Enables or disables hardware microphone noise reduction.
    pub async fn set_mic_noise_reduction(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        self.endpoint.call(5, [u8::from(enabled), 0, 0]).await?;
        Ok(())
    }
}

/// Parses `count` big-endian `u16` frequencies from a `getFrequencies` response,
/// which carries them starting at byte 1 (after the echoed band index).
fn parse_frequency_page(payload: &[u8; 16], count: u8) -> Result<Vec<u16>, Hidpp20Error> {
    let count = usize::from(count);
    if 1 + 2 * count > payload.len() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok((0..count)
        .map(|i| u16::from_be_bytes([payload[1 + 2 * i], payload[2 + 2 * i]]))
        .collect())
}

/// Parses `count` signed gains from a payload starting at `offset`.
fn parse_gains(payload: &[u8; 16], offset: usize, count: u8) -> Result<Vec<i8>, Hidpp20Error> {
    let count = usize::from(count);
    if offset + count > payload.len() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(payload[offset..offset + count]
        .iter()
        .map(|&byte| byte as i8)
        .collect())
}

//! Implements the `PerKeyLighting` feature (ID `0x8081`, version 0) that sets
//! individual RGB zones (typically per-key) on a keyboard.
//!
//! Zone updates are staged with the various `set_*_rgb_zones` functions and then
//! committed as a frame with [`frame_end`](PerKeyLightingFeature::frame_end).
//! Several setters trade addressing flexibility for the number of zones updated
//! per request; the delta-compression variants pack the most zones by sending
//! signed per-channel deltas from the previous frame.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Length of the zone-presence bitfield page returned by `getInfo`.
pub const ZONE_PRESENCE_PAGE_LEN: usize = 14;
/// Length of the packed payload for the delta-compression setters.
pub const DELTA_PACKED_LEN: usize = 15;
/// `typeOfInfo` value selecting the zone-presence query.
const TYPE_RGB_ZONE_PRESENCE: u8 = 0x00;
/// Maximum zones per `setIndividualRgbZones` request.
const MAX_INDIVIDUAL_ZONES: usize = 4;
/// Number of zones per `setConsecutiveRgbZones` request.
const CONSECUTIVE_ZONES: usize = 5;
/// Maximum ranges per `setRangeRgbZones` request.
const MAX_RANGES: usize = 3;
/// Maximum zones per `setRgbZonesSingleValue` request.
const MAX_SINGLE_VALUE_ZONES: usize = 13;

/// An 8-bit-per-channel RGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

/// A single zone and the color to apply to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RgbZone {
    /// Zone identifier (`0` and `255` are reserved end-of-list sentinels).
    pub zone_id: u8,
    /// Color to apply.
    pub color: Rgb,
}

/// A contiguous range of zones to fill with one color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RgbZoneRange {
    /// First zone identifier in the range (inclusive).
    pub first_zone_id: u8,
    /// Last zone identifier in the range (inclusive).
    pub last_zone_id: u8,
    /// Color to apply across the range.
    pub color: Rgb,
}

/// Which page of zone IDs a presence query covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ZonePresencePage {
    /// Zone IDs 0..=111.
    Zones0To111 = 0,
    /// Zone IDs 112..=223.
    Zones112To223 = 1,
    /// Zone IDs 224..=255.
    Zones224To255 = 2,
}

/// Storage persistence for [`frame_end`](PerKeyLightingFeature::frame_end).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum FramePersistence {
    /// Volatile: applied to RAM only.
    Volatile = 0,
    /// Applied to RAM and stored in EEPROM.
    VolatileAndNonVolatile = 1,
}

/// Implements the `PerKeyLighting` / `0x8081` feature.
#[derive(Clone)]
pub struct PerKeyLightingFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for PerKeyLightingFeature {
    const ID: u16 = 0x8081;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for PerKeyLightingFeature {}

impl PerKeyLightingFeature {
    /// Retrieves a page of the RGB zone-presence bitfield.
    ///
    /// The returned 14 bytes form a 112-bit field; bit `i` (LSB-first within each
    /// byte) reports whether the zone at `page` base `+ i` is present.
    pub async fn get_rgb_zone_presence(
        &self,
        page: ZonePresencePage,
    ) -> Result<[u8; ZONE_PRESENCE_PAGE_LEN], Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [TYPE_RGB_ZONE_PRESENCE, page.into(), 0])
            .await?
            .extend_payload();
        let mut bitfield = [0; ZONE_PRESENCE_PAGE_LEN];
        bitfield.copy_from_slice(&payload[2..2 + ZONE_PRESENCE_PAGE_LEN]);
        Ok(bitfield)
    }

    /// Sets up to four individually addressed zones.
    ///
    /// At most four zones are sent; extra entries are ignored.
    pub async fn set_individual_rgb_zones(&self, zones: &[RgbZone]) -> Result<(), Hidpp20Error> {
        validate_individual_zones(zones)?;
        self.endpoint
            .call_long(1, individual_zones_args(zones))
            .await?;
        Ok(())
    }

    /// Sets five consecutive zones starting at `first_zone_id`.
    pub async fn set_consecutive_rgb_zones(
        &self,
        first_zone_id: u8,
        colors: [Rgb; CONSECUTIVE_ZONES],
    ) -> Result<(), Hidpp20Error> {
        validate_zone_id(first_zone_id)?;
        self.endpoint
            .call_long(2, consecutive_zones_args(first_zone_id, colors))
            .await?;
        Ok(())
    }

    /// Sets eight consecutive zones from `first_zone_id` using 5-bit signed
    /// per-channel deltas.
    ///
    /// `packed` carries the 8×3 5-bit deltas packed MSB-first, zone-by-zone then
    /// channel-by-channel, exactly as defined by the feature spec; this wrapper
    /// transmits it verbatim.
    pub async fn set_consecutive_rgb_zones_delta_5bit(
        &self,
        first_zone_id: u8,
        packed: [u8; DELTA_PACKED_LEN],
    ) -> Result<(), Hidpp20Error> {
        self.send_delta(3, first_zone_id, packed).await
    }

    /// Sets ten consecutive zones from `first_zone_id` using 4-bit signed
    /// per-channel deltas.
    ///
    /// `packed` carries the 10×3 4-bit signed deltas, two per byte (high nibble
    /// first), as defined by the feature spec; this wrapper transmits it verbatim.
    pub async fn set_consecutive_rgb_zones_delta_4bit(
        &self,
        first_zone_id: u8,
        packed: [u8; DELTA_PACKED_LEN],
    ) -> Result<(), Hidpp20Error> {
        self.send_delta(4, first_zone_id, packed).await
    }

    /// Sets up to three independent ranges, each filled with one color.
    ///
    /// At most three ranges are sent; extra entries are ignored.
    pub async fn set_range_rgb_zones(&self, ranges: &[RgbZoneRange]) -> Result<(), Hidpp20Error> {
        validate_ranges(ranges)?;
        self.endpoint.call_long(5, range_zones_args(ranges)).await?;
        Ok(())
    }

    /// Applies one color to up to thirteen individually addressed zones.
    ///
    /// At most thirteen zone IDs are sent; extra entries are ignored.
    pub async fn set_rgb_zones_single_value(
        &self,
        color: Rgb,
        zone_ids: &[u8],
    ) -> Result<(), Hidpp20Error> {
        validate_single_value_zones(zone_ids)?;
        self.endpoint
            .call_long(6, single_value_args(color, zone_ids))
            .await?;
        Ok(())
    }

    /// Commits all pending zone changes and updates the display.
    ///
    /// `current_frame` and `frames_till_next_change` drive frame animations; pass
    /// `0` for both for a one-shot update.
    pub async fn frame_end(
        &self,
        persistence: FramePersistence,
        current_frame: u16,
        frames_till_next_change: u16,
    ) -> Result<(), Hidpp20Error> {
        let args = frame_end_args(persistence, current_frame, frames_till_next_change);
        self.endpoint.call_long(7, args).await?;
        Ok(())
    }

    /// Shared body of the delta-compression setters.
    async fn send_delta(
        &self,
        function: u8,
        first_zone_id: u8,
        packed: [u8; DELTA_PACKED_LEN],
    ) -> Result<(), Hidpp20Error> {
        validate_zone_id(first_zone_id)?;
        self.endpoint
            .call_long(function, delta_args(first_zone_id, packed))
            .await?;
        Ok(())
    }
}

fn validate_zone_id(zone_id: u8) -> Result<(), Hidpp20Error> {
    if matches!(zone_id, 0 | 0xff) {
        return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
    }
    Ok(())
}

fn validate_individual_zones(zones: &[RgbZone]) -> Result<(), Hidpp20Error> {
    for zone in zones.iter().take(MAX_INDIVIDUAL_ZONES) {
        validate_zone_id(zone.zone_id)?;
    }
    Ok(())
}

fn validate_ranges(ranges: &[RgbZoneRange]) -> Result<(), Hidpp20Error> {
    for range in ranges.iter().take(MAX_RANGES) {
        validate_zone_id(range.first_zone_id)?;
        validate_zone_id(range.last_zone_id)?;
    }
    Ok(())
}

fn validate_single_value_zones(zone_ids: &[u8]) -> Result<(), Hidpp20Error> {
    for &zone_id in zone_ids.iter().take(MAX_SINGLE_VALUE_ZONES) {
        validate_zone_id(zone_id)?;
    }
    Ok(())
}

/// Encodes a `setIndividualRgbZones` request.
fn individual_zones_args(zones: &[RgbZone]) -> [u8; 16] {
    let mut args = [0; 16];
    for (slot, zone) in zones.iter().take(MAX_INDIVIDUAL_ZONES).enumerate() {
        let base = slot * 4;
        args[base] = zone.zone_id;
        args[base + 1] = zone.color.red;
        args[base + 2] = zone.color.green;
        args[base + 3] = zone.color.blue;
    }
    args
}

/// Encodes a `setConsecutiveRgbZones` request.
fn consecutive_zones_args(first_zone_id: u8, colors: [Rgb; CONSECUTIVE_ZONES]) -> [u8; 16] {
    let mut args = [0; 16];
    args[0] = first_zone_id;
    for (i, color) in colors.iter().enumerate() {
        let base = 1 + i * 3;
        args[base] = color.red;
        args[base + 1] = color.green;
        args[base + 2] = color.blue;
    }
    args
}

/// Encodes a `setRangeRgbZones` request.
fn range_zones_args(ranges: &[RgbZoneRange]) -> [u8; 16] {
    let mut args = [0; 16];
    for (slot, range) in ranges.iter().take(MAX_RANGES).enumerate() {
        let base = slot * 5;
        args[base] = range.first_zone_id;
        args[base + 1] = range.last_zone_id;
        args[base + 2] = range.color.red;
        args[base + 3] = range.color.green;
        args[base + 4] = range.color.blue;
    }
    args
}

/// Encodes a `setRgbZonesSingleValue` request.
fn single_value_args(color: Rgb, zone_ids: &[u8]) -> [u8; 16] {
    let mut args = [0; 16];
    args[0] = color.red;
    args[1] = color.green;
    args[2] = color.blue;
    for (i, &zone_id) in zone_ids.iter().take(MAX_SINGLE_VALUE_ZONES).enumerate() {
        args[3 + i] = zone_id;
    }
    args
}

/// Encodes a `frameEnd` request.
fn frame_end_args(
    persistence: FramePersistence,
    current_frame: u16,
    frames_till_next_change: u16,
) -> [u8; 16] {
    let [frame_hi, frame_lo] = current_frame.to_be_bytes();
    let [next_hi, next_lo] = frames_till_next_change.to_be_bytes();
    let mut args = [0; 16];
    args[..5].copy_from_slice(&[persistence.into(), frame_hi, frame_lo, next_hi, next_lo]);
    args
}

/// Encodes a delta-compression request body (`first_zone_id` + packed deltas).
fn delta_args(first_zone_id: u8, packed: [u8; DELTA_PACKED_LEN]) -> [u8; 16] {
    let mut args = [0; 16];
    args[0] = first_zone_id;
    args[1..1 + DELTA_PACKED_LEN].copy_from_slice(&packed);
    args
}

//! Implements the `FeatureSet` feature (ID `0x0001`) that allows enumerating
//! all the features supported by a device.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureType},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `FeatureSet` / `0x0001` feature.
///
/// This feature is primarily used to collect all features supported by the
/// device. To achieve this, call [`Self::count`] to retrieve the amount of
/// supported features (excluding the root feature). Then call
/// [`Self::get_feature`] for every `i in 1..=count` (1-based, as accessing the
/// root feature is not allowed).
#[derive(Clone)]
pub struct FeatureSetFeature {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device to implement the feature for.
    device_index: u8,

    /// The index of the feature in the feature table.
    feature_index: u8,
}

impl CreatableFeature for FeatureSetFeature {
    const ID: u16 = 0x0001;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }
}

impl Feature for FeatureSetFeature {}

impl FeatureSetFeature {
    /// Retrieves the amount of features supported by the device, not including
    /// the root feature.
    pub async fn count(&self) -> Result<u8, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(0),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;

        Ok(response.extend_payload()[0])
    }

    /// Retrieves the information about a specific feature based on its index in
    /// the feature table.
    ///
    /// Feature index `0` for the root feature is not allowed.
    pub async fn get_feature(&self, index: u8) -> Result<FeatureInformation, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(1),
                    software_id: self.chan.get_sw_id(),
                },
                [index, 0x00, 0x00],
            ))
            .await?;

        let payload = response.extend_payload();

        Ok(FeatureInformation {
            id: (payload[0] as u16) << 8 | payload[1] as u16,
            typ: FeatureType::from(payload[2]),
            version: payload[3],
        })
    }
}

/// Represents information about a specific feature as returned by the
/// [`FeatureSetFeature::get_feature`] function.
#[derive(Clone, Copy, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FeatureInformation {
    /// The protocol ID of the feature.
    pub id: u16,

    /// The type of the feature.
    pub typ: FeatureType,

    /// The latest supported version of the feature.
    ///
    /// Multi-version features are always backwards compatible as long as the
    /// feature ID does not change, meaning functions implemented for an older
    /// version of the same feature will behave as expected for every later
    /// version.
    ///
    /// This field was added in feature version 1 and will be `0` for all older
    /// versions.
    pub version: u8,
}

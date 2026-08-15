//! Implements the `FeatureSet` feature (ID `0x0001`) that allows enumerating
//! all the features supported by a device.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, FeatureType},
    protocol::v20::Hidpp20Error,
};

/// Implements the `FeatureSet` / `0x0001` feature.
///
/// This feature is primarily used to collect all features supported by the
/// device. To achieve this, call [`Self::count`] to retrieve the amount of
/// supported features (excluding the root feature). Then call
/// [`Self::get_feature`] for every `i in 1..=count` (1-based, as accessing the
/// root feature is not allowed).
#[derive(Clone, Feature)]
#[creatable(id = 0x0001, version = 0)]
pub struct FeatureSetFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl FeatureSetFeature {
    /// Retrieves the amount of features supported by the device, not including
    /// the root feature.
    pub async fn count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Retrieves the information about a specific feature based on its index in
    /// the feature table.
    ///
    /// Feature index `0` for the root feature is not allowed.
    pub async fn get_feature(&self, index: u8) -> Result<FeatureInformation, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [index, 0x00, 0x00])
            .await?
            .extend_payload();

        Ok(FeatureInformation {
            id: u16::from(payload[0]) << 8 | u16::from(payload[1]),
            typ: FeatureType::from_bits_retain(payload[2]),
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

//! Implements the `Root` feature (ID `0x0000`) that every device supports by
//! default.

use std::sync::Arc;

use super::{CreatableFeature, Feature, FeatureType};
use crate::{
    channel::HidppChannel,
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `Root` / `0x0000` feature that every HID++2.0 device
/// supports by default.
///
/// This implementation is added automatically to any [`crate::device::Device`]
/// created using [`crate::device::Device::new`].
#[derive(Clone)]
pub struct RootFeature {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device to implement the feature for.
    device_index: u8,
}

impl CreatableFeature for RootFeature {
    const ID: u16 = 0x0000;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, _: u8) -> Self {
        Self { chan, device_index }
    }
}

impl Feature for RootFeature {}

impl RootFeature {
    /// Retrieves information about a specific feature ID, including its index
    /// in the feature table, its type and its version.
    ///
    /// If the feature is not supported by the device, [`None`] is returned.
    ///
    /// If the device only supports the root feature version 1, the
    /// [`FeatureInformation::version`] field will be `0` for all features.
    pub async fn get_feature(&self, id: u16) -> Result<Option<FeatureInformation>, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: 0,
                    function_id: U4::from_lo(0),
                    software_id: self.chan.get_sw_id(),
                },
                [(id >> 8) as u8, id as u8, 0x00],
            ))
            .await?;

        let payload = response.extend_payload();
        if payload[0] == 0 {
            return Ok(None);
        }

        Ok(Some(FeatureInformation {
            index: payload[0],
            typ: FeatureType::from(payload[1]),
            version: payload[2],
        }))
    }

    /// Pings the device with an arbitrary data byte. The device will respond
    /// with the same data if communication succeeds.
    ///
    /// The underlying function, as described in the protocol specification,
    /// will also look up the protocol version supported by the device.\
    /// This is not implemented here, as the
    /// [`crate::protocol::determine_version`] function does so in a more
    /// general manner.
    pub async fn ping(&self, data: u8) -> Result<u8, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: 0,
                    function_id: U4::from_lo(1),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, data],
            ))
            .await?;

        let payload = response.extend_payload();
        Ok(payload[2])
    }
}

/// Represents information about a specific feature as returned by the
/// [`RootFeature::get_feature`] function.
#[derive(Clone, Copy, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FeatureInformation {
    /// The index of the feature in the version table.
    /// This is used for invocations of functions of that feature.
    pub index: u8,

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

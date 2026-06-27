//! Implements the legacy `ReportRate` feature (ID `0x8060`).

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Report-rate values supported by a `0x8060` device, encoded as milliseconds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ReportRateList: u8 {
        /// 1 ms report interval.
        const MS_1 = 1 << 0;
        /// 2 ms report interval.
        const MS_2 = 1 << 1;
        /// 3 ms report interval.
        const MS_3 = 1 << 2;
        /// 4 ms report interval.
        const MS_4 = 1 << 3;
        /// 5 ms report interval.
        const MS_5 = 1 << 4;
        /// 6 ms report interval.
        const MS_6 = 1 << 5;
        /// 7 ms report interval.
        const MS_7 = 1 << 6;
        /// 8 ms report interval.
        const MS_8 = 1 << 7;
    }
}

/// Implements the `ReportRate` / `0x8060` feature.
#[derive(Clone)]
pub struct ReportRateFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for ReportRateFeature {
    const ID: u16 = 0x8060;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for ReportRateFeature {}

impl ReportRateFeature {
    /// Retrieves the supported report intervals in milliseconds.
    pub async fn get_report_rate_list(&self) -> Result<ReportRateList, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ReportRateList::from_bits_retain(payload[0]))
    }

    /// Retrieves the active report interval in milliseconds.
    pub async fn get_report_rate(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(1, [0; 3]).await?.extend_payload()[0])
    }

    /// Sets the active report interval in milliseconds.
    ///
    /// Devices reject unsupported intervals with `InvalidArgument`.
    pub async fn set_report_rate(&self, report_rate_ms: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [report_rate_ms, 0, 0]).await?;
        Ok(())
    }
}

//! Implements `ExtendedAdjustableReportRate` (feature `0x8061`).

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Report-rate values supported by a `0x8061` device.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ExtendedReportRateList: u16 {
        /// 125 Hz, equivalent to an 8 ms report interval.
        const HZ_125 = 1 << 0;
        /// 250 Hz, equivalent to a 4 ms report interval.
        const HZ_250 = 1 << 1;
        /// 500 Hz, equivalent to a 2 ms report interval.
        const HZ_500 = 1 << 2;
        /// 1000 Hz, equivalent to a 1 ms report interval.
        const HZ_1000 = 1 << 3;
        /// 2000 Hz, equivalent to a 500 µs report interval.
        const HZ_2000 = 1 << 4;
        /// 4000 Hz, equivalent to a 250 µs report interval.
        const HZ_4000 = 1 << 5;
        /// 8000 Hz, equivalent to a 125 µs report interval.
        const HZ_8000 = 1 << 6;
    }
}

/// A connection type used by `ExtendedAdjustableReportRate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ConnectionType {
    /// Wired USB connection.
    Wired = 0,
    /// Logitech gaming wireless connection.
    GamingWireless = 1,
}

/// A concrete report-rate setting for `0x8061`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ExtendedReportRate {
    /// 125 Hz, equivalent to an 8 ms report interval.
    Hz125 = 0,
    /// 250 Hz, equivalent to a 4 ms report interval.
    Hz250 = 1,
    /// 500 Hz, equivalent to a 2 ms report interval.
    Hz500 = 2,
    /// 1000 Hz, equivalent to a 1 ms report interval.
    Hz1000 = 3,
    /// 2000 Hz, equivalent to a 500 µs report interval.
    Hz2000 = 4,
    /// 4000 Hz, equivalent to a 250 µs report interval.
    Hz4000 = 5,
    /// 8000 Hz, equivalent to a 125 µs report interval.
    Hz8000 = 6,
}

/// Implements the `ExtendedAdjustableReportRate` / `0x8061` feature.
#[derive(Clone)]
pub struct ExtendedReportRateFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for ExtendedReportRateFeature {
    const ID: u16 = 0x8061;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for ExtendedReportRateFeature {}

impl ExtendedReportRateFeature {
    /// Retrieves the report rates supported by `connection_type`.
    pub async fn get_device_capabilities(
        &self,
        connection_type: ConnectionType,
    ) -> Result<ExtendedReportRateList, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [u8::from(connection_type), 0, 0])
            .await?
            .extend_payload();
        Ok(report_rate_list_from_payload(payload))
    }

    /// Retrieves the report rates available for the device's current connection.
    pub async fn get_actual_report_rate_list(
        &self,
    ) -> Result<ExtendedReportRateList, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(report_rate_list_from_payload(payload))
    }

    /// Retrieves the active report rate for `connection_type`.
    pub async fn get_report_rate(
        &self,
        connection_type: ConnectionType,
    ) -> Result<ExtendedReportRate, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [u8::from(connection_type), 0, 0])
            .await?
            .extend_payload();
        ExtendedReportRate::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the report rate for the current host-side connection.
    pub async fn set_report_rate(
        &self,
        report_rate: ExtendedReportRate,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call(3, [u8::from(report_rate), 0, 0]).await?;
        Ok(())
    }
}

fn report_rate_list_from_payload(payload: [u8; 16]) -> ExtendedReportRateList {
    ExtendedReportRateList::from_bits_retain(u16::from_be_bytes([payload[0], payload[1]]))
}

#[cfg(test)]
mod tests {
    use super::{ExtendedReportRateList, report_rate_list_from_payload};

    #[test]
    fn parses_report_rate_mask() {
        let mut payload = [0; 16];
        payload[1] = 0b0100_1001;

        let rates = report_rate_list_from_payload(payload);

        assert!(rates.contains(ExtendedReportRateList::HZ_125));
        assert!(rates.contains(ExtendedReportRateList::HZ_1000));
        assert!(rates.contains(ExtendedReportRateList::HZ_8000));
    }
}

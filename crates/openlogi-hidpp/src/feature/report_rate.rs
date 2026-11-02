//! Implements the legacy `ReportRate` feature (ID `0x8060`).

use std::ops::RangeInclusive;

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// The report intervals `0x8060` can express, in milliseconds: one bit each
/// in [`ReportRateList`].
const REPORT_RATE_MS: RangeInclusive<u8> = 1..=8;

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
#[derive(Clone, Feature)]
#[creatable(id = 0x8060, version = 0)]
pub struct ReportRateFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl ReportRateFeature {
    /// Retrieves the supported report intervals in milliseconds.
    pub async fn get_report_rate_list(&self) -> Result<ReportRateList, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ReportRateList::from_bits_retain(payload[0]))
    }

    /// Retrieves the active report interval in milliseconds.
    ///
    /// A reply outside `1..=8` ms is not an interval this feature can report,
    /// so it fails with [`Hidpp20Error::UnsupportedResponse`] rather than
    /// passing on as a rate.
    pub async fn get_report_rate(&self) -> Result<u8, Hidpp20Error> {
        let report_rate_ms = self.endpoint.call(1, [0; 3]).await?.extend_payload()[0];
        if REPORT_RATE_MS.contains(&report_rate_ms) {
            Ok(report_rate_ms)
        } else {
            Err(Hidpp20Error::UnsupportedResponse)
        }
    }

    /// Sets the active report interval in milliseconds.
    ///
    /// An interval outside `1..=8` ms fails with `InvalidArgument` before
    /// anything is sent. Devices reject an in-range interval they do not
    /// support with the same error.
    pub async fn set_report_rate(&self, report_rate_ms: u8) -> Result<(), Hidpp20Error> {
        if !REPORT_RATE_MS.contains(&report_rate_ms) {
            return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
        }
        self.endpoint.call(2, [report_rate_ms, 0, 0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{assert_matches, sync::Arc};

    use super::ReportRateFeature;
    use crate::{
        channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
        feature::CreatableFeature,
        nibble::U4,
        protocol::v20::{ErrorType, Hidpp20Error, Message, MessageHeader},
    };

    const DEVICE: u8 = 3;
    const FEATURE_INDEX: u8 = 8;

    fn header(function: u8) -> MessageHeader {
        MessageHeader {
            device_index: DEVICE,
            feature_index: FEATURE_INDEX,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        }
    }

    async fn feature() -> (ReportRateFeature, MockRawHidHandle) {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        (
            ReportRateFeature::new(channel, DEVICE, FEATURE_INDEX),
            handle,
        )
    }

    /// `getReportRate` answering `reply_ms`.
    fn read_rate(reply_ms: u8) -> Result<u8, Hidpp20Error> {
        futures::executor::block_on(async {
            let (feature, handle) = feature().await;
            let mut pending = Box::pin(feature.get_report_rate());
            assert!(futures::poll!(pending.as_mut()).is_pending());
            handle
                .send_incoming(Message::Short(header(1), [reply_ms, 0, 0]).into())
                .await;
            pending.await
        })
    }

    #[test]
    fn the_active_rate_is_one_to_eight_ms() {
        assert_eq!(read_rate(1).ok(), Some(1));
        assert_eq!(read_rate(8).ok(), Some(8));
        for invalid in [0, 9, 255] {
            assert_matches!(read_rate(invalid), Err(Hidpp20Error::UnsupportedResponse));
        }
    }

    #[test]
    fn an_out_of_range_rate_is_refused_before_it_is_sent() {
        futures::executor::block_on(async {
            let (feature, handle) = feature().await;
            for invalid in [0, 9, 255] {
                assert_matches!(
                    feature.set_report_rate(invalid).await,
                    Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
                );
            }
            assert!(handle.written_reports().is_empty());

            let mut pending = Box::pin(feature.set_report_rate(8));
            assert!(futures::poll!(pending.as_mut()).is_pending());
            handle
                .send_incoming(Message::Short(header(2), [8, 0, 0]).into())
                .await;
            assert_matches!(pending.await, Ok(()));
            let written = handle.written_reports();
            assert_eq!(written.len(), 1);
            assert_eq!(written[0][4], 8);
        });
    }
}

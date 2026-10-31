//! The USB report-rate register (`0x64`) of HID++ 1.0 gaming mice.
//!
//! Reverse engineered; follows libratbag's `hidpp10_get_usb_refresh_rate`
//! (`src/hidpp10.c`, MIT). The first byte is the interval between reports in
//! milliseconds. The onboard profiles of the G500, G700 and G9 store the
//! same byte (see [`super::onboard_memory::g700::G700Profile`]), so both
//! decode through [`ReportInterval`].

use std::num::NonZeroU8;

use crate::{channel::HidppChannel, protocol::v10::Hidpp10Error};

/// The register's address.
const REGISTER: u8 = 0x64;

/// The interval between two reports, in whole milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ReportInterval(NonZeroU8);

impl ReportInterval {
    /// The interval in milliseconds.
    #[must_use]
    pub fn millis(self) -> u8 {
        self.0.get()
    }

    /// The report rate in hertz, rounded down: 1 ms is 1000 Hz, 3 ms 333 Hz.
    #[must_use]
    pub fn rate_hz(self) -> u16 {
        1000 / u16::from(self.0.get())
    }

    /// The three bytes a short `SET_REGISTER` to `0x64` carries.
    #[must_use]
    pub fn to_register(self) -> [u8; 3] {
        [self.0.get(), 0x00, 0x00]
    }
}

impl TryFrom<u8> for ReportInterval {
    type Error = Hidpp10Error;

    /// A zero interval names no rate; libratbag refuses it too.
    fn try_from(millis: u8) -> Result<Self, Self::Error> {
        NonZeroU8::new(millis)
            .map(Self)
            .ok_or(Hidpp10Error::UnsupportedResponse)
    }
}

/// Reads the report interval of the device at `device_index`.
pub async fn read_report_interval(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<ReportInterval, Hidpp10Error> {
    let [millis, ..] = chan.read_register(device_index, REGISTER, [0; 3]).await?;
    ReportInterval::try_from(millis)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ReportInterval, read_report_interval};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::{Hidpp10Error, MessageType},
    };

    #[test]
    fn the_rate_is_the_interval_inverted_and_rounded_down() {
        for (millis, hz) in [(1, 1000), (2, 500), (3, 333), (4, 250), (8, 125), (255, 3)] {
            let interval = ReportInterval::try_from(millis).unwrap();
            assert_eq!(interval.rate_hz(), hz, "{millis} ms");
            assert_eq!(interval.to_register(), [millis, 0, 0]);
        }
    }

    #[test]
    fn a_zero_interval_is_rejected() {
        assert!(matches!(
            ReportInterval::try_from(0),
            Err(Hidpp10Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn reads_the_first_register_byte() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(HidppMessage::Short([
                1,
                MessageType::GetRegister.into(),
                0x64,
                0x04,
                0x00,
                0x00,
            ]));

            let interval = read_report_interval(&channel, 1).await.unwrap();
            assert_eq!(interval.millis(), 4);
            assert_eq!(interval.rate_hz(), 250);
        });
    }
}

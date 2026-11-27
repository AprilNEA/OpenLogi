//! The current-resolution register (`0x63`) of HID++ 1.0 gaming mice, in its
//! long form (G500, G700).
//!
//! Reverse engineered; follows libratbag's `hidpp10_get_current_resolution`
//! and `hidpp10_set_current_resolution` (`src/hidpp10.c`, MIT). The register
//! is read with a short `GET_LONG_REGISTER` request and answered with a long
//! report whose first four bytes are the X and Y values, little-endian, and
//! written with a long `SET_LONG_REGISTER` carrying the same four bytes. The
//! G9 uses a short form instead, which this module does not cover.
//!
//! The values are raw sensor-table steps, not DPI: the conversion is the
//! device's (for the G700, [`super::onboard_memory::g700::dpi_from_raw`]).

use crate::{channel::HidppChannel, protocol::v10::Hidpp10Error};

/// The register's address.
const REGISTER: u8 = 0x63;

/// The resolution the sensor runs at right now, in raw sensor-table steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Resolution {
    /// The X-axis step.
    pub x: u16,
    /// The Y-axis step.
    pub y: u16,
}

impl Resolution {
    /// Decodes the register's long reply.
    #[must_use]
    pub fn from_register(data: [u8; 16]) -> Self {
        let [x_lo, x_hi, y_lo, y_hi, ..] = data;
        Self {
            x: u16::from_le_bytes([x_lo, x_hi]),
            y: u16::from_le_bytes([y_lo, y_hi]),
        }
    }

    /// The sixteen bytes a `SET_LONG_REGISTER` to `0x63` carries to select
    /// this resolution; the bytes after the two values are zero, as
    /// libratbag sends them.
    #[must_use]
    pub fn to_register(self) -> [u8; 16] {
        let mut data = [0; 16];
        data[..2].copy_from_slice(&self.x.to_le_bytes());
        data[2..4].copy_from_slice(&self.y.to_le_bytes());
        data
    }
}

/// Reads the resolution the device at `device_index` runs at.
pub async fn read_current_resolution(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<Resolution, Hidpp10Error> {
    chan.read_long_register(device_index, REGISTER, [0; 3])
        .await
        .map(Resolution::from_register)
}

/// Makes the device at `device_index` run at `resolution`. It lasts until the
/// device switches DPI mode or profile; the onboard profile is not changed.
pub async fn write_current_resolution(
    chan: &HidppChannel,
    device_index: u8,
    resolution: Resolution,
) -> Result<(), Hidpp10Error> {
    chan.write_long_register(device_index, REGISTER, resolution.to_register())
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Resolution, read_current_resolution, write_current_resolution};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::MessageType,
    };

    #[test]
    fn the_long_form_carries_both_axes_little_endian() {
        let mut data = [0; 16];
        data[..4].copy_from_slice(&[0x24, 0x00, 0x45, 0x01]);
        let resolution = Resolution::from_register(data);
        assert_eq!(resolution, Resolution { x: 0x24, y: 0x0145 });
        assert_eq!(resolution.to_register(), data);
    }

    #[test]
    fn reads_through_a_short_get_long_request() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let mut reply = [0u8; 19];
            reply[..7].copy_from_slice(&[
                1,
                MessageType::GetLongRegister.into(),
                0x63,
                0x24,
                0x00,
                0x24,
                0x00,
            ]);
            handle.queue_response(HidppMessage::Long(reply));

            let resolution = read_current_resolution(&channel, 1).await.unwrap();
            assert_eq!(resolution, Resolution { x: 0x24, y: 0x24 });
            assert_eq!(
                handle.written_reports()[0],
                [0x10, 1, 0x83, 0x63, 0, 0, 0],
                "the request is a short report even though the reply is long"
            );
        });
    }

    #[test]
    fn writes_through_a_long_set_long_request() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            // The device acknowledges a long write with a short report.
            handle.queue_response(HidppMessage::Short([
                1,
                MessageType::SetLongRegister.into(),
                0x63,
                0x00,
                0x00,
                0x00,
            ]));

            write_current_resolution(&channel, 1, Resolution { x: 0x24, y: 0x0145 })
                .await
                .unwrap();

            let mut expected = vec![0x11, 1, 0x82, 0x63, 0x24, 0x00, 0x45, 0x01];
            expected.resize(20, 0);
            assert_eq!(handle.written_reports(), [expected]);
        });
    }
}

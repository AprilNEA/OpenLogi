//! Implements the `AdcMeasurement` feature (ID `0x1F20`) that reports a
//! device's battery as a measured voltage plus a link/charging flags byte.
//!
//! Wireless G-series headsets expose `0x1F20` and none of `0x1000` / `0x1001`
//! / `0x1004` (verified on a G733; the Linux kernel reads a G935 the same
//! way), so without this feature the inventory probe finds no battery source
//! for them. Like `BatteryVoltage` (`0x1001`) the feature reports no
//! percentage — callers estimate one from the voltage.
//!
//! `getAdcMeasurement` (function `0`) reads the battery. The firmware also
//! broadcasts the same payload unsolicited as event `0` when the reading or
//! the link changes, which
//! [`AdcMeasurementFeature::is_status_broadcast`] recognizes.
//!
//! Functions `1` and `2` read and write the headset's automatic power-off
//! timeout in minutes, `0` meaning never. Solaar (`ADCPower`) documents them
//! for version 2 and later; a G933 (`046d:0a5b`) answers both at version 0
//! (measured on the device). A device that reports the feature without them
//! answers function `1` with a HID++ error.
//!
//! The wire layout is not in a public Logitech spec: the voltage as a
//! big-endian millivolt `u16` followed by one flags byte was
//! reverse-engineered. The flag values follow the Linux kernel
//! (`hid-logitech-hidpp.c`, `hidpp20_map_adc_measurement_1f20`): bit `0` means
//! the device is linked, bit `1` charging, bit `2` charge complete.

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Implements the `AdcMeasurement` / `0x1F20` feature.
#[derive(Feature)]
#[creatable(id = 0x1f20, version = 0)]
pub struct AdcMeasurementFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl AdcMeasurementFeature {
    /// Whether an unsolicited message with `function_id` is this feature's
    /// status broadcast (event `0`), sent when the reading or the link
    /// changes.
    ///
    /// Deliberately independent of the payload: the broadcast announces a
    /// change even when its flags value is one [`AdcMeasurement::from_wire`]
    /// rejects (the kernel names `0x0F`), so a listener that only needs to know
    /// *that* something changed must not drop it.
    #[must_use]
    pub const fn is_status_broadcast(function_id: u8) -> bool {
        function_id == 0
    }

    /// Reads the measured battery voltage and link/charging state (function
    /// `0`, `getAdcMeasurement`).
    ///
    /// A `LogitechInternal` (`0x05`) error answer is [`AdcMeasurement::Unlinked`]:
    /// a G733 dongle whose headset is switched off answers that instead of a
    /// clear link bit (captured from hardware), and the kernel reads a protocol
    /// error from this call as the device being offline. Other errors stay
    /// errors, so a `Busy` answer does not pass for a switched-off device.
    pub async fn get_adc_measurement(&self) -> Result<AdcMeasurement, Hidpp20Error> {
        match self.endpoint.call(0, [0; 3]).await {
            Ok(response) => AdcMeasurement::from_wire(&response.extend_payload()),
            Err(Hidpp20Error::Feature(ErrorType::LogitechInternal)) => Ok(AdcMeasurement::Unlinked),
            Err(e) => Err(e),
        }
    }

    /// Reads the automatic power-off timeout in minutes, `0` meaning never
    /// (function `1`).
    pub async fn get_power_off_timeout(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(1, [0; 3]).await?.extend_payload()[0])
    }

    /// Sets the automatic power-off timeout in minutes, `0` meaning never
    /// (function `2`), and returns the timeout the device echoes.
    ///
    /// The echo is the confirmation to use, as with the sidetone level: a
    /// G933 echoes the value it stored, and reads it back afterwards
    /// (measured on the device).
    pub async fn set_power_off_timeout(&self, minutes: u8) -> Result<u8, Hidpp20Error> {
        Ok(self
            .endpoint
            .call(2, [minutes, 0, 0])
            .await?
            .extend_payload()[0])
    }
}

/// A reading from the `0x1F20` `getAdcMeasurement` function, or its event `0`
/// broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum AdcMeasurement {
    /// The battery-powered end is not linked — flags bit `0` clear, or the
    /// `LogitechInternal` error a G733 dongle answers while its headset is
    /// off — so there is no reading.
    Unlinked,
    /// A live battery reading.
    Linked {
        /// Measured battery voltage in millivolt.
        voltage_mv: u16,
        /// The charging state decoded from the flags byte.
        status: AdcChargingStatus,
    },
}

impl AdcMeasurement {
    /// Decodes a `getAdcMeasurement` response payload: voltage as a big-endian
    /// millivolt `u16` in bytes `0`–`1`, the flags in byte `2`.
    ///
    /// # Errors
    ///
    /// [`Hidpp20Error::UnsupportedResponse`] for a linked flags value outside
    /// the known set (the kernel's explicit `0x0F` included).
    pub fn from_wire(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        let flags = payload[2];
        if flags & 0x01 == 0 {
            return Ok(Self::Unlinked);
        }
        Ok(Self::Linked {
            voltage_mv: u16::from_be_bytes([payload[0], payload[1]]),
            status: AdcChargingStatus::try_from(flags)
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }
}

/// Charging state carried by a linked `0x1F20` flags byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum AdcChargingStatus {
    /// Running on battery.
    Discharging = 0x01,
    /// On external power and charging.
    Charging = 0x03,
    /// On external power with charge complete.
    Full = 0x07,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{AdcChargingStatus, AdcMeasurement, AdcMeasurementFeature};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
        },
        feature::CreatableFeature,
        nibble::U4,
        protocol::v20::{ErrorType, Hidpp20Error, Message, MessageHeader},
    };

    /// The `0x1F20` feature index on a G933.
    const ADC_INDEX: u8 = 8;

    async fn device() -> (AdcMeasurementFeature, MockRawHidHandle) {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        (AdcMeasurementFeature::new(channel, 0xff, ADC_INDEX), handle)
    }

    fn reply(function: u8, minutes: u8) -> Message {
        let header = MessageHeader {
            device_index: 0xff,
            feature_index: ADC_INDEX,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        Message::Short(header, [minutes, 0, 0])
    }

    #[test]
    fn reads_the_power_off_timeout_with_function_one() {
        futures::executor::block_on(async {
            let (feature, handle) = device().await;
            // A G933 set to 15 minutes answers `0f`.
            handle.queue_response(reply(1, 0x0f).into());
            assert_eq!(feature.get_power_off_timeout().await.unwrap(), 15);
            assert_eq!(
                handle.written_reports(),
                [vec![0x10, 0xff, ADC_INDEX, 0x11, 0, 0, 0]]
            );
        });
    }

    #[test]
    fn a_power_off_write_returns_the_timeout_the_device_echoes() {
        futures::executor::block_on(async {
            let (feature, handle) = device().await;
            // The G933 echoed `1e` for a 30-minute write.
            handle.queue_response(reply(2, 0x1e).into());
            assert_eq!(feature.set_power_off_timeout(30).await.unwrap(), 30);
            assert_eq!(
                handle.written_reports(),
                [vec![0x10, 0xff, ADC_INDEX, 0x21, 30, 0, 0]]
            );
        });
    }

    #[test]
    fn a_device_without_the_power_off_functions_answers_an_error() {
        futures::executor::block_on(async {
            let (feature, handle) = device().await;
            // The device rejects feature 8, function/software byte 0x11,
            // with INVALID_FUNCTION_ID (0x07).
            handle.queue_response(HidppMessage::Short([0xff, 0xff, ADC_INDEX, 0x11, 0x07, 0]));
            assert!(matches!(
                feature.get_power_off_timeout().await,
                Err(Hidpp20Error::Feature(ErrorType::InvalidFunctionId))
            ));
        });
    }

    /// Builds a 16-byte payload from the 3 meaningful bytes.
    fn payload(voltage_mv: u16, flags: u8) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..2].copy_from_slice(&voltage_mv.to_be_bytes());
        payload[2] = flags;
        payload
    }

    #[test]
    fn g733_reading_decodes_voltage_and_status() {
        // Captured from a G733 dongle (046d:0ab5): `0f 41 01`.
        let mut wire = [0; 16];
        wire[..3].copy_from_slice(&[0x0f, 0x41, 0x01]);
        assert!(matches!(
            AdcMeasurement::from_wire(&wire),
            Ok(AdcMeasurement::Linked {
                voltage_mv: 3905,
                status: AdcChargingStatus::Discharging,
            })
        ));
    }

    #[test]
    fn known_flag_values_map_to_charging_states() {
        for (flags, status) in [
            (0x01, AdcChargingStatus::Discharging),
            (0x03, AdcChargingStatus::Charging),
            (0x07, AdcChargingStatus::Full),
        ] {
            let decoded = AdcMeasurement::from_wire(&payload(4000, flags));
            assert!(
                matches!(
                    decoded,
                    Ok(AdcMeasurement::Linked { voltage_mv: 4000, status: s }) if s == status
                ),
                "flags {flags:#04x} decoded as {decoded:?}"
            );
        }
    }

    #[test]
    fn clear_link_bit_is_unlinked_whatever_else_is_set() {
        assert!(matches!(
            AdcMeasurement::from_wire(&payload(0, 0x00)),
            Ok(AdcMeasurement::Unlinked)
        ));
        assert!(matches!(
            AdcMeasurement::from_wire(&payload(3900, 0x06)),
            Ok(AdcMeasurement::Unlinked)
        ));
    }

    #[test]
    fn unknown_linked_flags_are_an_error() {
        for flags in [0x05, 0x0f, 0x81] {
            let decoded = AdcMeasurement::from_wire(&payload(3900, flags));
            assert!(
                matches!(decoded, Err(Hidpp20Error::UnsupportedResponse)),
                "flags {flags:#04x} decoded as {decoded:?}"
            );
        }
    }
}

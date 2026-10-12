//! The battery registers of HID++ 1.0 peripherals.
//!
//! A device implements one of two: [`BatteryRegister::Status`] reports a
//! coarse level, [`BatteryRegister::Charge`] a percentage. Neither layout is in
//! a published Logitech spec. Both are reverse engineered and follow Solaar's
//! `parse_battery_status` (`lib/logitech_receiver/hidpp10.py`).

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    protocol::v10::{ErrorType, Hidpp10Error},
};

/// A HID++ 1.0 register that reports a device's battery.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BatteryRegister {
    /// `0x07`: a coarse level and a charging state. Solaar lists it for the
    /// Performance MX and the M510.
    Status = 0x07,
    /// `0x0D`: a percentage and a charging state. Solaar lists it for the
    /// M705 (M-R0009).
    Charge = 0x0d,
}

impl BatteryRegister {
    /// Whether the device at `device_index` implements this register.
    ///
    /// `Ok(false)` only for [`ErrorType::InvalidAddress`], the device's own
    /// answer for a register it lacks. Any other error — the receiver
    /// reporting the device asleep ([`ErrorType::ResourceError`]) or busy, or
    /// no reply at all — says nothing about the device and is returned.
    pub async fn is_implemented(
        self,
        chan: &HidppChannel,
        device_index: u8,
    ) -> Result<bool, Hidpp10Error> {
        match chan.read_register(device_index, self.into(), [0; 3]).await {
            Ok(_) => Ok(true),
            Err(Hidpp10Error::RegisterAccess(ErrorType::InvalidAddress)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Reads [`BatteryRegister::Status`].
pub async fn read_status(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<StatusReading, Hidpp10Error> {
    chan.read_register(device_index, BatteryRegister::Status.into(), [0; 3])
        .await?
        .try_into()
}

/// Reads [`BatteryRegister::Charge`].
pub async fn read_charge(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<ChargeReading, Hidpp10Error> {
    chan.read_register(device_index, BatteryRegister::Charge.into(), [0; 3])
        .await?
        .try_into()
}

/// A reading of [`BatteryRegister::Status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct StatusReading {
    /// The charge level, or `None` when the level byte is zero, which Solaar
    /// notes a charging report can carry instead of a level.
    pub level: Option<StatusLevel>,
    /// The charging state.
    pub charging: ChargingState,
}

impl TryFrom<[u8; 3]> for StatusReading {
    type Error = Hidpp10Error;

    fn try_from([level, charging, _]: [u8; 3]) -> Result<Self, Self::Error> {
        let level = match level {
            0 => None,
            level => {
                Some(StatusLevel::try_from(level).map_err(|_| Hidpp10Error::UnsupportedResponse)?)
            }
        };
        Ok(Self {
            level,
            charging: ChargingState::from_status_byte(charging)?,
        })
    }
}

/// The coarse charge level [`BatteryRegister::Status`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum StatusLevel {
    /// Almost depleted.
    Critical = 1,
    /// Running low.
    Low = 3,
    /// Comfortable.
    Good = 5,
    /// At or near full charge.
    Full = 7,
}

/// A reading of [`BatteryRegister::Charge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ChargeReading {
    /// The charge, `0..=100`.
    pub percentage: u8,
    /// The charging state.
    pub charging: ChargingState,
}

impl TryFrom<[u8; 3]> for ChargeReading {
    type Error = Hidpp10Error;

    fn try_from([percentage, _, charging]: [u8; 3]) -> Result<Self, Self::Error> {
        if percentage > 100 {
            return Err(Hidpp10Error::UnsupportedResponse);
        }
        Ok(Self {
            percentage,
            charging: ChargingState::from_charge_byte(charging)?,
        })
    }
}

/// The charging state both battery registers report, each in its own encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ChargingState {
    /// Running on battery.
    Discharging,
    /// Charging.
    Recharging,
    /// Charge complete while still on power.
    Full,
}

impl ChargingState {
    /// Decodes [`BatteryRegister::Status`]'s second byte. Its bits are
    /// undocumented: Solaar tests the `0x21` mask (recharging) before `0x22`
    /// (full), and so does this.
    fn from_status_byte(byte: u8) -> Result<Self, Hidpp10Error> {
        if byte == 0 {
            Ok(Self::Discharging)
        } else if byte & 0x21 == 0x21 {
            Ok(Self::Recharging)
        } else if byte & 0x22 == 0x22 {
            Ok(Self::Full)
        } else {
            Err(Hidpp10Error::UnsupportedResponse)
        }
    }

    /// Decodes the high nibble of [`BatteryRegister::Charge`]'s third byte.
    fn from_charge_byte(byte: u8) -> Result<Self, Hidpp10Error> {
        match byte & 0xf0 {
            0x30 => Ok(Self::Discharging),
            0x50 => Ok(Self::Recharging),
            0x90 => Ok(Self::Full),
            _ => Err(Hidpp10Error::UnsupportedResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        BatteryRegister, ChargeReading, ChargingState, StatusLevel, StatusReading, read_status,
    };
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::{ErrorType, Hidpp10Error, MessageType},
    };

    #[test]
    fn status_decodes_each_level_and_charging_state() {
        for (wire, level, charging) in [
            (
                [0x07, 0x00, 0x00],
                Some(StatusLevel::Full),
                ChargingState::Discharging,
            ),
            (
                [0x05, 0x00, 0x00],
                Some(StatusLevel::Good),
                ChargingState::Discharging,
            ),
            (
                [0x03, 0x21, 0x00],
                Some(StatusLevel::Low),
                ChargingState::Recharging,
            ),
            (
                [0x01, 0x25, 0x00],
                Some(StatusLevel::Critical),
                ChargingState::Recharging,
            ),
            (
                [0x07, 0x22, 0x00],
                Some(StatusLevel::Full),
                ChargingState::Full,
            ),
            ([0x00, 0x21, 0x00], None, ChargingState::Recharging),
        ] {
            assert_eq!(
                StatusReading::try_from(wire).unwrap(),
                StatusReading { level, charging },
                "{wire:02x?}"
            );
        }
    }

    #[test]
    fn status_tests_the_recharging_mask_first() {
        // 0x23 carries both masks; Solaar reads it as recharging.
        let reading = StatusReading::try_from([0x05, 0x23, 0x00]).unwrap();
        assert_eq!(reading.charging, ChargingState::Recharging);
    }

    #[test]
    fn status_rejects_unknown_levels_and_charging_bytes() {
        for wire in [[0x02, 0x00, 0x00], [0x08, 0x00, 0x00], [0x05, 0x01, 0x00]] {
            assert!(
                matches!(
                    StatusReading::try_from(wire),
                    Err(Hidpp10Error::UnsupportedResponse)
                ),
                "{wire:02x?}"
            );
        }
    }

    #[test]
    fn charge_decodes_the_percentage_and_the_state_nibble() {
        for (wire, percentage, charging) in [
            ([0x4b, 0x00, 0x30], 75, ChargingState::Discharging),
            ([0x0a, 0x00, 0x5f], 10, ChargingState::Recharging),
            ([0x64, 0x00, 0x90], 100, ChargingState::Full),
        ] {
            assert_eq!(
                ChargeReading::try_from(wire).unwrap(),
                ChargeReading {
                    percentage,
                    charging
                },
                "{wire:02x?}"
            );
        }
    }

    #[test]
    fn charge_rejects_an_impossible_percentage_or_unknown_state() {
        for wire in [[0x65, 0x00, 0x30], [0x32, 0x00, 0x10]] {
            assert!(
                matches!(
                    ChargeReading::try_from(wire),
                    Err(Hidpp10Error::UnsupportedResponse)
                ),
                "{wire:02x?}"
            );
        }
    }

    fn register_reply(device: u8, address: u8, data: [u8; 3]) -> HidppMessage {
        HidppMessage::Short([
            device,
            MessageType::GetRegister.into(),
            address,
            data[0],
            data[1],
            data[2],
        ])
    }

    fn register_error(device: u8, address: u8, error: ErrorType) -> HidppMessage {
        HidppMessage::Short([
            device,
            MessageType::Error.into(),
            MessageType::GetRegister.into(),
            address,
            error.into(),
            0x00,
        ])
    }

    #[test]
    fn only_an_invalid_address_means_the_register_is_absent() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);

            handle.queue_response(register_reply(1, 0x07, [0x05, 0x00, 0x00]));
            assert!(
                BatteryRegister::Status
                    .is_implemented(&channel, 1)
                    .await
                    .unwrap()
            );

            handle.queue_response(register_error(1, 0x0d, ErrorType::InvalidAddress));
            assert!(
                !BatteryRegister::Charge
                    .is_implemented(&channel, 1)
                    .await
                    .unwrap()
            );

            // The receiver answering for a sleeping device proves nothing
            // about which registers the device has.
            handle.queue_response(register_error(1, 0x0d, ErrorType::ResourceError));
            assert!(matches!(
                BatteryRegister::Charge.is_implemented(&channel, 1).await,
                Err(Hidpp10Error::RegisterAccess(ErrorType::ResourceError))
            ));
        });
    }

    #[test]
    fn read_status_decodes_the_device_reply() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(register_reply(2, 0x07, [0x03, 0x00, 0x00]));

            let reading = read_status(&channel, 2).await.unwrap();
            assert_eq!(reading.level, Some(StatusLevel::Low));
            assert_eq!(reading.charging, ChargingState::Discharging);
        });
    }
}

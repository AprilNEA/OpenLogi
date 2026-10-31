//! The current-profile register (`0x0F`) of HID++ 1.0 gaming mice.
//!
//! The G500, G700 and G9 keep up to five profiles in onboard flash (see
//! [`super::onboard_memory`]) and play the one this register selects. The
//! layout is reverse engineered and follows libratbag's
//! `hidpp10_get_current_profile` (`src/hidpp10.c`, MIT): the first byte says
//! how the profile is addressed, the other two carry the address.

use crate::{channel::HidppChannel, protocol::v10::Hidpp10Error};

/// The register's address.
const REGISTER: u8 = 0x0f;

/// Addressing byte: the profile's position in the profile directory.
const BY_INDEX: u8 = 0x00;
/// Addressing byte: the profile's location in onboard memory.
const BY_ADDRESS: u8 = 0x01;
/// Addressing byte: the firmware's built-in profile, which also stops the
/// profile buttons from switching.
const FACTORY: u8 = 0xff;

/// The profile the mouse plays, as the register reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum CurrentProfile {
    /// The profile at this position of the profile directory, counted from 0.
    Index(u8),
    /// The profile stored at this location of onboard memory. `offset` is
    /// the directory entry's offset byte; libratbag matches the pair against
    /// the directory and never interprets the offset on its own.
    Address {
        /// The memory page.
        page: u8,
        /// The offset within the page, in the directory's unit.
        offset: u8,
    },
    /// The firmware's built-in profile. Profile switching is off while it
    /// plays.
    Factory,
}

impl CurrentProfile {
    /// The three register bytes that select this profile, the payload a
    /// short `SET_REGISTER` to `0x0F` carries.
    #[must_use]
    pub fn to_register(self) -> [u8; 3] {
        match self {
            Self::Index(index) => [BY_INDEX, index, 0x00],
            Self::Address { page, offset } => [BY_ADDRESS, page, offset],
            Self::Factory => [FACTORY, 0x00, 0x00],
        }
    }
}

impl TryFrom<[u8; 3]> for CurrentProfile {
    type Error = Hidpp10Error;

    /// Decodes the register's reply. libratbag also names an EEPROM
    /// addressing byte (`0xEE`) without ever reading it; that and any other
    /// unknown byte is [`Hidpp10Error::UnsupportedResponse`].
    fn try_from([kind, first, second]: [u8; 3]) -> Result<Self, Self::Error> {
        match kind {
            BY_INDEX => Ok(Self::Index(first)),
            BY_ADDRESS => Ok(Self::Address {
                page: first,
                offset: second,
            }),
            FACTORY => Ok(Self::Factory),
            _ => Err(Hidpp10Error::UnsupportedResponse),
        }
    }
}

/// Reads the profile the device at `device_index` plays.
pub async fn read_current_profile(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<CurrentProfile, Hidpp10Error> {
    chan.read_register(device_index, REGISTER, [0; 3])
        .await?
        .try_into()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{CurrentProfile, read_current_profile};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::{ErrorType, Hidpp10Error, MessageType},
    };

    #[test]
    fn decodes_each_addressing_kind() {
        for (wire, profile) in [
            ([0x00, 0x02, 0x00], CurrentProfile::Index(2)),
            (
                [0x01, 0x03, 0x00],
                CurrentProfile::Address { page: 3, offset: 0 },
            ),
            ([0xff, 0x00, 0x00], CurrentProfile::Factory),
        ] {
            assert_eq!(
                CurrentProfile::try_from(wire).unwrap(),
                profile,
                "{wire:02x?}"
            );
            assert_eq!(profile.to_register(), wire, "{profile:?}");
        }
    }

    #[test]
    fn rejects_an_unknown_addressing_byte() {
        for wire in [[0x02, 0x00, 0x00], [0xee, 0x00, 0x00]] {
            assert!(
                matches!(
                    CurrentProfile::try_from(wire),
                    Err(Hidpp10Error::UnsupportedResponse)
                ),
                "{wire:02x?}"
            );
        }
    }

    #[test]
    fn reads_the_register_and_surfaces_device_errors() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);

            handle.queue_response(HidppMessage::Short([
                1,
                MessageType::GetRegister.into(),
                0x0f,
                0x00,
                0x02,
                0x00,
            ]));
            assert_eq!(
                read_current_profile(&channel, 1).await.unwrap(),
                CurrentProfile::Index(2)
            );
            assert_eq!(
                handle.written_reports()[0],
                [0x10, 1, 0x81, 0x0f, 0, 0, 0],
                "a short GET_REGISTER with empty parameters"
            );

            handle.queue_response(HidppMessage::Short([
                1,
                MessageType::Error.into(),
                MessageType::GetRegister.into(),
                0x0f,
                ErrorType::InvalidAddress.into(),
                0x00,
            ]));
            assert!(matches!(
                read_current_profile(&channel, 1).await,
                Err(Hidpp10Error::RegisterAccess(ErrorType::InvalidAddress))
            ));
        });
    }
}

//! Implements the different HID++ wireless receivers.
//!
//! Because of the lack of public documentation about the different receivers
//! and their capabilities, and because I currently only own a single Bolt
//! receiver, this module is largely incomplete. I would be more than happy for
//! anyone who owns a different receiver, with Unifying having the highest
//! priority, and who is willing to actively support its implementation by
//! providing information and testing.
//!
//! Receivers can generally only be differentiated by their USB vendor and
//! product IDs, so [`detect`] dispatches through the shared
//! `openlogi-device-registry`.

use std::sync::Arc;

use openlogi_device_registry::receiver::{ReceiverProtocol, find_receiver};
use thiserror::Error;

use crate::{channel::HidppChannel, protocol::v10::Hidpp10Error};

pub mod bolt;
pub mod unifying;

/// The index to use when communicating with the receiver on any HID++ channel.
pub const RECEIVER_DEVICE_INDEX: u8 = 0xff;

/// HID++ 1.0 receiver register holding [`NotificationFlags`].
const NOTIFICATIONS_REGISTER: u8 = 0x00;

/// The protocol byte (byte 3 of a `0x41` device-connection notification) of
/// a Logi Bolt link.
const BOLT_PROTOCOL: u8 = 0x10;

/// Whether a `0x41` device-connection notification reports an encrypted link.
///
/// Bit 5 of the device-info byte is the link encryption flag. A link whose
/// protocol byte is Bolt is encrypted even when that bit is clear: Solaar's
/// `notification_information`, which decodes every Unifying-style receiver,
/// uses `data[0] & 0x20 or address == 0x10`.
pub(crate) const fn connection_encrypted(protocol: u8, device_info: u8) -> bool {
    device_info & (1 << 5) != 0 || protocol == BOLT_PROTOCOL
}

bitflags::bitflags! {
    /// Which notifications a Bolt or Unifying receiver sends: register `0x00`,
    /// a 24-bit big-endian word.
    ///
    /// Other host software (Options+, Solaar) sets bits this crate does not
    /// name, so values are built from a register read (unknown bits retained)
    /// and only the bits a caller owns are edited before writing it back.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub struct NotificationFlags: u32 {
        /// Device arrival/removal (`0x41`) notifications.
        const WIRELESS = 0x00_0100;
        /// Host software present. Receivers stream pairing events only while
        /// this and [`Self::WIRELESS`] are set.
        const SOFTWARE_PRESENT = 0x00_0800;
    }
}

impl NotificationFlags {
    fn from_register(value: [u8; 3]) -> Self {
        Self::from_bits_retain(u32::from_be_bytes([0, value[0], value[1], value[2]]))
    }

    fn to_register(self) -> [u8; 3] {
        let [_, high, mid, low] = self.bits().to_be_bytes();
        [high, mid, low]
    }
}

/// Read a receiver's notification flags.
pub async fn read_notification_flags(
    chan: &HidppChannel,
) -> Result<NotificationFlags, Hidpp10Error> {
    chan.read_register(RECEIVER_DEVICE_INDEX, NOTIFICATIONS_REGISTER, [0; 3])
        .await
        .map(NotificationFlags::from_register)
}

/// Write a receiver's notification flags. `flags` must derive from
/// [`read_notification_flags`]: the write replaces every bit.
pub async fn write_notification_flags(
    chan: &HidppChannel,
    flags: NotificationFlags,
) -> Result<(), Hidpp10Error> {
    chan.write_register(
        RECEIVER_DEVICE_INDEX,
        NOTIFICATIONS_REGISTER,
        flags.to_register(),
    )
    .await
}

/// Tries to detect the receiver present on a HID++ channel.
pub fn detect(chan: Arc<HidppChannel>) -> Option<Receiver> {
    match find_receiver(chan.vendor_id, chan.product_id)?.protocol {
        ReceiverProtocol::Bolt => bolt::Receiver::new(chan).ok().map(Receiver::Bolt),
        ReceiverProtocol::Unifying => unifying::Receiver::new(chan).ok().map(Receiver::Unifying),
    }
}

/// Represents a HID++ wireless receiver.
#[derive(Clone)]
#[non_exhaustive]
pub enum Receiver {
    /// Logi Bolt receiver.
    Bolt(bolt::Receiver),
    /// Logitech Unifying receiver.
    Unifying(unifying::Receiver),
}

impl Receiver {
    /// Provides a human-readable name for the receiver.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Bolt(_) => "Logi Bolt Receiver",
            Self::Unifying(_) => "Unifying Receiver",
        }
        .to_string()
    }

    /// Provides a string that uniquely identifies the specific receiver.
    ///
    /// This MAY be the serial number, but it may also be any other value that
    /// is defined as unique.
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        match self {
            Self::Bolt(bolt) => bolt.get_unique_id().await,
            Self::Unifying(unifying) => unifying.get_unique_id().await,
        }
    }
}

/// Represents an error returned by a receiver.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReceiverError {
    /// Indicates that no supported receiver could be identified on a HID++
    /// channel.
    #[error("no (supported) receiver could be found")]
    UnknownReceiver,

    /// Indicates that a HID++1.0 register access resulted in an error.
    #[error("a HID++1.0 error occurred")]
    Protocol(#[from] Hidpp10Error),
}

#[cfg(test)]
mod tests {
    use super::NotificationFlags;

    #[test]
    fn notification_flags_round_trip_unknown_bits() {
        // Battery-status (0x10_0000) and an unnamed low bit set by another host.
        let flags = NotificationFlags::from_register([0x10, 0x01, 0x55]);
        assert!(flags.contains(NotificationFlags::WIRELESS));
        assert!(!flags.contains(NotificationFlags::SOFTWARE_PRESENT));
        assert_eq!(flags.to_register(), [0x10, 0x01, 0x55]);
        assert_eq!(
            (flags | NotificationFlags::SOFTWARE_PRESENT).to_register(),
            [0x10, 0x09, 0x55]
        );
        assert_eq!(
            (flags - NotificationFlags::WIRELESS).to_register(),
            [0x10, 0x00, 0x55]
        );
    }
}

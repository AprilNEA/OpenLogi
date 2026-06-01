//! Implements the Unifying Receiver.
//!
//! Unifying is a very versatile receiver that can pair up to 6 supported
//! devices.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    receiver::{RECEIVER_DEVICE_INDEX, ReceiverError},
};

/// All USB vendor & product ID pairs that are known to identify Unifying
/// receivers.
pub const VPID_PAIRS: &[(u16, u16)] = &[(0x046d, 0xc52b), (0x046d, 0xc532)];

/// All known registers of the Unifying receiver.
///
/// In most cases you should not need to access these manually, as [`Receiver`]
/// implements many features.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Register {
    /// Provides information about the receiver and paired devices. It uses
    /// sub-registers, as defined in [`UnifyingInfoSubRegister`], to
    /// differentiate between different kinds of information.
    ReceiverInfo = 0xb5,
}

/// Represents the known sub-registers of the [`UnifyingRegister::ReceiverInfo`]
/// register.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum InfoSubRegister {
    /// Provides general information about the receiver.
    ReceiverInfo = 0x03,
}

/// Implements the Unifying wireless receiver.
#[derive(Clone)]
pub struct Receiver {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,
}

impl Receiver {
    /// Tries to initialize a new [`UnifyingReceiver`] from a raw HID++ channel.
    ///
    /// If no receiver could be found, or if the vendor and product IDs don't
    /// match the ones of any known Unifying receiver, this function will return
    /// [`ReceiverError::UnknownReceiver`].
    pub fn new(chan: Arc<HidppChannel>) -> Result<Self, ReceiverError> {
        if !VPID_PAIRS.contains(&(chan.vendor_id, chan.product_id)) {
            return Err(ReceiverError::UnknownReceiver);
        }

        Ok(Receiver { chan })
    }

    /// Provides general information about the receiver.
    pub async fn get_receiver_info(&self) -> Result<ReceiverInfo, ReceiverError> {
        let response = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [InfoSubRegister::ReceiverInfo.into(), 0, 0],
            )
            .await?;

        Ok(ReceiverInfo {
            serial_number: hex::encode_upper(&response[1..=4]),
            pairing_slots: response[6],
        })
    }
}

/// Represents some general information about a Unifying receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReceiverInfo {
    pub serial_number: String,
    pub pairing_slots: u8,
}

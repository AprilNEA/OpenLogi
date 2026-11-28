//! Implements receivers that speak HID++ 2.0 themselves.
//!
//! Newer Lightspeed receivers (e.g. `c54f`, shipped with the PRO X3
//! SUPERSTRIKE) answer HID++ 2.0 at [`RECEIVER_DEVICE_INDEX`] and reject the
//! HID++ 1.0 receiver registers (`0x02`, `0xB5`) with error `0x06`. They send
//! no `0x41` connection notification either, so a paired device is found by
//! pinging its slot: an online device answers, while an empty slot and an
//! offline device both stay silent and cannot be told apart.
//!
//! No public spec covers these receivers: everything above was observed on a
//! `c54f` with a PRO X3 SUPERSTRIKE on slot 1.
//!
//! The receiver exposes undocumented features (`0x1893`, `0x1894`, `0x1895`)
//! that likely cover pairing; they are left untouched here.

use std::sync::Arc;

use openlogi_device_registry::receiver::{ReceiverProtocol, find_receiver};

use super::{RECEIVER_DEVICE_INDEX, ReceiverError};
use crate::{
    channel::{AbandonedReply, HidppChannel},
    feature::{CreatableFeature, device_information::DeviceInformationFeature, root::RootFeature},
    nibble::U4,
    protocol::v20::{Message, MessageHeader},
};

/// The byte a slot ping asks the device to echo back.
const PING_DATA: u8 = 0x5a;

/// Highest pairing slot a HID++ 2.0 receiver is pinged at. Observed on
/// `c54f`: slot `7` answers with an error, slots `1..=6` stay silent when
/// empty.
pub const MAX_SLOT: u8 = 6;

/// Implements a receiver that speaks HID++ 2.0.
#[derive(Clone)]
pub struct Receiver {
    chan: Arc<HidppChannel>,
}

impl Receiver {
    /// Tries to initialize a new [`Receiver`] from a raw HID++ channel.
    ///
    /// Returns [`ReceiverError::UnknownReceiver`] when the channel's VID/PID
    /// doesn't match any known HID++ 2.0 receiver.
    pub fn new(chan: Arc<HidppChannel>) -> Result<Self, ReceiverError> {
        if find_receiver(chan.vendor_id, chan.product_id)
            .is_none_or(|receiver| receiver.protocol != ReceiverProtocol::Hidpp20)
        {
            return Err(ReceiverError::UnknownReceiver);
        }
        Ok(Self { chan })
    }

    /// Provides the unique ID of the receiver: the unit ID its own
    /// `DeviceInformation` (`0x0003`) feature reports.
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        let root = RootFeature::new(Arc::clone(&self.chan), RECEIVER_DEVICE_INDEX, 0);
        let Some(info) = root.get_feature(DeviceInformationFeature::ID).await? else {
            return Err(ReceiverError::UnknownReceiver);
        };
        let device_information = DeviceInformationFeature::new(
            Arc::clone(&self.chan),
            RECEIVER_DEVICE_INDEX,
            info.index,
        );
        let unit_id = device_information.get_device_info().await?.unit_id;
        Ok(hex::encode_upper(unit_id))
    }

    /// Pings the device at `slot`. Resolves only once the device answers: an
    /// empty or offline slot never replies, so the caller bounds this with its
    /// own timeout.
    ///
    /// A ping abandoned that way would otherwise reserve its header for
    /// [`STALE_REPLY_GRACE`](crate::channel::STALE_REPLY_GRACE) and hold back
    /// the next scan's identical ping until the grace passes. Its answer — the
    /// echoed byte — cannot be changed by any write, so a late reply may
    /// answer the re-ask ([`AbandonedReply::AdoptIdentical`]).
    pub async fn ping_slot(&self, slot: u8) -> Result<(), ReceiverError> {
        let ping = Message::Short(
            MessageHeader {
                device_index: slot,
                feature_index: 0,
                function_id: U4::from_lo(1),
                software_id: self.chan.get_sw_id(),
            },
            [0, 0, PING_DATA],
        );
        self.chan
            .send_v20_with(ping, AbandonedReply::AdoptIdentical)
            .await?;
        Ok(())
    }
}

//! Implements the `DeviceFriendlyName` feature (ID `0x0007`) that provides
//! functionality to set and retrieve a custom device name.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `DeviceFriendlyName` / `0x0007` feature.
#[derive(Clone)]
pub struct DeviceFriendlyNameFeature {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device to implement the feature for.
    device_index: u8,

    /// The index of the feature in the feature table.
    feature_index: u8,
}

impl CreatableFeature for DeviceFriendlyNameFeature {
    const ID: u16 = 0x0007;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }
}

impl Feature for DeviceFriendlyNameFeature {}

impl DeviceFriendlyNameFeature {
    /// Retrieves the length data of the friendly device name feature.
    pub async fn get_friendly_name_length(&self) -> Result<DeviceFriendlyNameLength, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(0),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;

        let payload = response.extend_payload();

        Ok(DeviceFriendlyNameLength {
            name_length: payload[0],
            name_max_length: payload[1],
            default_name_length: payload[2],
        })
    }

    /// Retrieves a chunk of characters of the friendly name of the device,
    /// starting at a specific index (inclusive).
    ///
    /// This function will always retrieve 15 bytes, filling up the rest with
    /// zeroes if the chunk is shorter than that.
    ///
    /// Use this function in conjunction with [`Self::get_friendly_name_length`]
    /// to retrieve the whole friendly name of the device.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_friendly_name`].
    pub async fn get_friendly_name(&self, index: u8) -> Result<[u8; 15], Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(1),
                    software_id: self.chan.get_sw_id(),
                },
                [index, 0x00, 0x00],
            ))
            .await?;

        Ok(response.extend_payload()[1..].try_into().unwrap())
    }

    /// Retrieves the whole friendly name of the device by first calling
    /// [`Self::get_friendly_name_length`] once and then repeatedly calling
    /// [`Self::get_friendly_name`] until all characters were received.
    pub async fn get_whole_friendly_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_friendly_name_length().await?.name_length;
        let mut string = String::with_capacity(count as usize);

        let mut len = 0;
        while len < count as usize {
            let part = self.get_friendly_name(len as u8).await?;
            string.push_str(str::from_utf8(&part).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
            len = string.len();
        }

        Ok(string.trim_end_matches(char::from(0)).to_string())
    }

    /// Retrieves a chunk of characters of the default friendly name of the
    /// device, starting at a specific index (inclusive).
    ///
    /// This function will always retrieve 15 bytes, filling up the rest with
    /// zeroes if the chunk is shorter than that.
    ///
    /// Use this function in conjunction with [`Self::get_friendly_name_length`]
    /// to retrieve the whole default friendly name of the device.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_default_friendly_name`].
    pub async fn get_default_friendly_name(&self, index: u8) -> Result<[u8; 15], Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(2),
                    software_id: self.chan.get_sw_id(),
                },
                [index, 0x00, 0x00],
            ))
            .await?;

        Ok(response.extend_payload()[1..].try_into().unwrap())
    }

    /// Retrieves the whole default friendly name of the device by first calling
    /// [`Self::get_friendly_name_length`] once and then repeatedly calling
    /// [`Self::get_default_friendly_name`] until all characters were received.
    pub async fn get_whole_default_friendly_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_friendly_name_length().await?.default_name_length;
        let mut string = String::with_capacity(count as usize);

        let mut len = 0;
        while len < count as usize {
            let part = self.get_default_friendly_name(len as u8).await?;
            string.push_str(str::from_utf8(&part).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
            len = string.len();
        }

        Ok(string.trim_end_matches(char::from(0)).to_string())
    }

    /// Sets a chunk of the friendly device name, starting at a specific index
    /// (inclusive).
    ///
    /// If the index and chunk combination would exceed the
    /// [`DeviceFriendlyNameLength::name_max_length`], the name is automatically
    /// truncated by the device.
    ///
    /// Returns the new total length of the friendly device name.
    ///
    /// A convenience wrapper setting the whole friendly device name at once is
    /// provided as [`Self::set_whole_device_name`].
    pub async fn set_friendly_name(&self, index: u8, chunk: [u8; 15]) -> Result<u8, Hidpp20Error> {
        let mut data = [0u8; 16];
        data[0] = index;
        data[1..].copy_from_slice(&chunk);

        let response = self
            .chan
            .send_v20(v20::Message::Long(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(3),
                    software_id: self.chan.get_sw_id(),
                },
                data,
            ))
            .await?;

        Ok(response.extend_payload()[0])
    }

    /// Sets the whole friendly device name, truncating the value to a maximum
    /// of [`DeviceFriendlyNameLength::name_max_length`] bytes.
    ///
    /// This method calls [`Self::get_friendly_name_length`] first to retrieve
    /// the maximum length and then repeatedly calls [`Self::set_friendly_name`]
    /// until the whole name is set.
    ///
    /// Returns the total length of the name after setting it,
    pub async fn set_whole_device_name(&self, name: String) -> Result<u8, Hidpp20Error> {
        let max_len = self.get_friendly_name_length().await?.name_max_length;
        let mut bytes = name.into_bytes();
        bytes.truncate(max_len as usize);
        let chunks = bytes.chunks_exact(15);
        let remainder = chunks.remainder();

        let mut index = 0;
        for chunk in chunks {
            index += self
                .set_friendly_name(index, chunk.try_into().unwrap())
                .await?;
        }

        if !remainder.is_empty() {
            let mut chunk = [0u8; 15];
            chunk[..remainder.len()].copy_from_slice(remainder);
            index += self.set_friendly_name(index, chunk).await?;
        }

        Ok(index)
    }

    /// Resets the friendly device name to the default one.
    ///
    /// Returns the total length of the name after resetting it,
    pub async fn reset_friendly_name(&self) -> Result<u8, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(4),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;

        Ok(response.extend_payload()[0])
    }
}

/// Represents the length data as returned by
/// [`DeviceFriendlyNameFeature::get_friendly_name_length`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceFriendlyNameLength {
    /// The current length of the friendly device name.
    pub name_length: u8,

    /// The maximum length of the friendly device name.
    pub name_max_length: u8,

    /// The length of the default friendly device name.
    pub default_name_length: u8,
}

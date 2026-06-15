//! Implements the `DeviceTypeAndName` feature (ID `0x0005`) that provides some
//! information about the marketing type and name of a device.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `DeviceTypeAndName` / `0x0005` feature.
#[derive(Clone)]
pub struct DeviceTypeAndNameFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for DeviceTypeAndNameFeature {
    const ID: u16 = 0x0005;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for DeviceTypeAndNameFeature {}

impl DeviceTypeAndNameFeature {
    /// Retrieves the amount of characters in the marketing name of the device.
    pub async fn get_device_name_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Retrieves a chunk of characters of the marketing name of the device,
    /// starting at a specific index (inclusive).
    ///
    /// Depending on the device and channel capabilities, this function will
    /// return at most 3 or 16 characters of the device name.
    ///
    /// Use this function in conjunction with [`Self::get_device_name_count`] to
    /// retrieve the whole device name.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_device_name`].
    pub async fn get_device_name(&self, index: u8) -> Result<Vec<u8>, Hidpp20Error> {
        let response = self.endpoint.call(1, [index, 0x00, 0x00]).await?;

        match response {
            v20::Message::Long(_, payload) => Ok(payload.to_vec()),
            v20::Message::Short(_, payload) => Ok(payload.to_vec()),
        }
    }

    /// Retrieves the whole marketing name of the device by first calling
    /// [`Self::get_device_name_count`] once and then repeatedly calling
    /// [`Self::get_device_name`] until all characters were received.
    pub async fn get_whole_device_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_device_name_count().await?;
        let mut string = String::with_capacity(count as usize);

        let mut len = 0;
        while len < count as usize {
            let part = self.get_device_name(len as u8).await?;
            string.push_str(str::from_utf8(&part).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
            len = string.len();
        }

        Ok(string.trim_end_matches(char::from(0)).to_string())
    }

    /// Retrieves the marketing type of the device.
    pub async fn get_device_type(&self) -> Result<DeviceType, Hidpp20Error> {
        DeviceType::try_from(self.endpoint.call(2, [0; 3]).await?.extend_payload()[0])
            .map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

/// Represents the type of a HID++2.0 device as returned by the
/// [`DeviceTypeAndNameFeature`] feature.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceType {
    Keyboard = 0,
    RemoteControl = 1,
    Numpad = 2,
    Mouse = 3,
    Trackpad = 4,
    Trackball = 5,
    Presenter = 6,
    Receiver = 7,
    Headset = 8,
    Webcam = 9,
    SteeringWheel = 10,
    Joystick = 11,
    Gamepad = 12,
    Dock = 13,
    Speaker = 14,
    Microphone = 15,
    IlluminationLight = 16,
    ProgrammableController = 17,
    CarSimPedals = 18,
    Adapter = 19,
}

//! Implements the `Illumination` feature (ID `0x1990`) for devices with a
//! controllable illumination light (brightness in Lumens and color temperature
//! in Kelvin).
//!
//! Brightness and color temperature share the same control shape — an info
//! query, a value get/set, and a level-list get/set — exposed as two parallel
//! sets of methods. Feature version 1 adds the effective-maximum brightness
//! query and its events.
//!
//! All multi-byte fields in this feature are big-endian.

pub mod event;
pub mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use event::IlluminationEvent;
pub use types::{
    BrightnessClampedSource, ControlCapabilities, ControlInfo, IlluminationState, LevelConfig,
    SetLevels,
};

use self::types::{be16, illumination_state};
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

// Function ids. Color-temperature functions mirror the brightness ones offset by
// five, but they are spelled out for clarity.
const FN_GET_ILLUMINATION: u8 = 0;
const FN_SET_ILLUMINATION: u8 = 1;
const FN_GET_BRIGHTNESS_INFO: u8 = 2;
const FN_GET_BRIGHTNESS: u8 = 3;
const FN_SET_BRIGHTNESS: u8 = 4;
const FN_GET_BRIGHTNESS_LEVELS: u8 = 5;
const FN_SET_BRIGHTNESS_LEVELS: u8 = 6;
const FN_GET_COLOR_TEMPERATURE_INFO: u8 = 7;
const FN_GET_COLOR_TEMPERATURE: u8 = 8;
const FN_SET_COLOR_TEMPERATURE: u8 = 9;
const FN_GET_COLOR_TEMPERATURE_LEVELS: u8 = 10;
const FN_SET_COLOR_TEMPERATURE_LEVELS: u8 = 11;
const FN_GET_BRIGHTNESS_EFFECTIVE_MAX: u8 = 12;

/// Implements the `Illumination` / `0x1990` feature.
pub struct IlluminationFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<IlluminationEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for IlluminationFeature {
    const ID: u16 = 0x1990;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                if let Some(event) = event::decode_event(func.to_lo(), &payload) {
                    emitter.emit(event);
                }
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for IlluminationFeature {}

impl EmittingFeature<IlluminationEvent> for IlluminationFeature {
    fn listen(&self) -> async_channel::Receiver<IlluminationEvent> {
        self.emitter.create_receiver()
    }
}

impl IlluminationFeature {
    /// Retrieves whether the illumination is on.
    pub async fn get_illumination(&self) -> Result<IlluminationState, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_ILLUMINATION, [0; 3])
            .await?
            .extend_payload();
        illumination_state(payload[0])
    }

    /// Turns the illumination on or off.
    pub async fn set_illumination(&self, state: IlluminationState) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_SET_ILLUMINATION, [u8::from(state), 0, 0])
            .await?;
        Ok(())
    }

    /// Retrieves the brightness capabilities and range (in Lumens).
    pub async fn get_brightness_info(&self) -> Result<ControlInfo, Hidpp20Error> {
        self.read_info(FN_GET_BRIGHTNESS_INFO).await
    }

    /// Retrieves the current brightness (in Lumens).
    pub async fn get_brightness(&self) -> Result<u16, Hidpp20Error> {
        self.read_value(FN_GET_BRIGHTNESS).await
    }

    /// Sets the brightness (in Lumens).
    ///
    /// The value must be within `[min, max]` and on the resolution grid from
    /// [`Self::get_brightness_info`]. On devices with a dynamic maximum a value
    /// above the effective maximum is clamped (see
    /// [`IlluminationEvent::BrightnessClamped`]).
    pub async fn set_brightness(&self, brightness: u16) -> Result<(), Hidpp20Error> {
        self.write_value(FN_SET_BRIGHTNESS, brightness).await
    }

    /// Retrieves the brightness level configuration starting at `start_index`
    /// (ignored for linear levels).
    pub async fn get_brightness_levels(
        &self,
        start_index: u8,
    ) -> Result<LevelConfig, Hidpp20Error> {
        self.read_levels(FN_GET_BRIGHTNESS_LEVELS, start_index)
            .await
    }

    /// Writes the brightness level configuration.
    pub async fn set_brightness_levels(&self, levels: &SetLevels) -> Result<(), Hidpp20Error> {
        self.write_levels(FN_SET_BRIGHTNESS_LEVELS, levels).await
    }

    /// Retrieves the current effective maximum brightness (in Lumens), or `0`
    /// when none is in effect. Requires feature version 1.
    pub async fn get_brightness_effective_max(&self) -> Result<u16, Hidpp20Error> {
        self.read_value(FN_GET_BRIGHTNESS_EFFECTIVE_MAX).await
    }

    /// Retrieves the color-temperature capabilities and range (in Kelvin).
    pub async fn get_color_temperature_info(&self) -> Result<ControlInfo, Hidpp20Error> {
        self.read_info(FN_GET_COLOR_TEMPERATURE_INFO).await
    }

    /// Retrieves the current color temperature (in Kelvin).
    pub async fn get_color_temperature(&self) -> Result<u16, Hidpp20Error> {
        self.read_value(FN_GET_COLOR_TEMPERATURE).await
    }

    /// Sets the color temperature (in Kelvin).
    ///
    /// The value must be within `[min, max]` and on the resolution grid from
    /// [`Self::get_color_temperature_info`].
    pub async fn set_color_temperature(&self, color_temperature: u16) -> Result<(), Hidpp20Error> {
        self.write_value(FN_SET_COLOR_TEMPERATURE, color_temperature)
            .await
    }

    /// Retrieves the color-temperature level configuration starting at
    /// `start_index` (ignored for linear levels).
    pub async fn get_color_temperature_levels(
        &self,
        start_index: u8,
    ) -> Result<LevelConfig, Hidpp20Error> {
        self.read_levels(FN_GET_COLOR_TEMPERATURE_LEVELS, start_index)
            .await
    }

    /// Writes the color-temperature level configuration.
    pub async fn set_color_temperature_levels(
        &self,
        levels: &SetLevels,
    ) -> Result<(), Hidpp20Error> {
        self.write_levels(FN_SET_COLOR_TEMPERATURE_LEVELS, levels)
            .await
    }

    /// Shared `get<Control>Info` reader.
    async fn read_info(&self, function: u8) -> Result<ControlInfo, Hidpp20Error> {
        let payload = self.endpoint.call(function, [0; 3]).await?.extend_payload();
        Ok(ControlInfo::from_payload(&payload))
    }

    /// Shared `get<Control>` / effective-max reader for a big-endian `u16`.
    async fn read_value(&self, function: u8) -> Result<u16, Hidpp20Error> {
        let payload = self.endpoint.call(function, [0; 3]).await?.extend_payload();
        Ok(be16(&payload, 0))
    }

    /// Shared `set<Control>` writer for a big-endian `u16`.
    async fn write_value(&self, function: u8, value: u16) -> Result<(), Hidpp20Error> {
        let [hi, lo] = value.to_be_bytes();
        self.endpoint.call(function, [hi, lo, 0]).await?;
        Ok(())
    }

    /// Shared `get<Control>Levels` reader.
    async fn read_levels(
        &self,
        function: u8,
        start_index: u8,
    ) -> Result<LevelConfig, Hidpp20Error> {
        // The request carries the start index in the high nibble of byte 0.
        let payload = self
            .endpoint
            .call(function, [start_index << 4, 0, 0])
            .await?
            .extend_payload();
        Ok(LevelConfig::from_payload(&payload))
    }

    /// Shared `set<Control>Levels` writer.
    async fn write_levels(&self, function: u8, levels: &SetLevels) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call_long(function, levels.to_payload())
            .await?;
        Ok(())
    }
}

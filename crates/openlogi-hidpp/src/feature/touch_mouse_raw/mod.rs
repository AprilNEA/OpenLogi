//! Implements the `TouchMouseRaw` feature (ID `0x6110`) that exposes a touch
//! mouse's raw touch points: pad characteristics, the raw-data mode, and the
//! raw-data / status events.

pub mod event;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use event::{TouchMousePoint, TouchMouseRawEvent, TouchMouseStatus};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

/// The position of the touch surface's coordinate origin, viewed from above.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Origin {
    /// Lower-left corner.
    LowerLeft = 1,
    /// Lower-right corner.
    LowerRight = 2,
    /// Upper-left corner.
    UpperLeft = 3,
    /// Upper-right corner.
    UpperRight = 4,
}

/// The raw-reporting mode of a touch mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RawMode {
    /// Native gestures only (out of the box).
    NativeGestures = 0,
    /// Filtered raw data.
    RawFiltered = 1,
    /// Unfiltered raw data plus native gestures.
    RawUnfilteredAndGestures = 2,
    /// Unfiltered raw data, sent even while lifted or with a button active.
    RawUnfilteredAlways = 3,
    /// Like [`RawUnfilteredAndGestures`](Self::RawUnfilteredAndGestures) but with
    /// Z information in place of width.
    RawUnfilteredWithZ = 4,
}

/// Touch-mouse characteristics from
/// [`get_touchpad_info`](TouchMouseRawFeature::get_touchpad_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TouchMouseInfo {
    /// Maximum X count in dots.
    pub x_max_count: u16,
    /// Maximum Y count in dots.
    pub y_max_count: u16,
    /// Sensor resolution in DPI (assumed equal for X and Y).
    pub resolution_dpi: u16,
    /// Position of the coordinate origin.
    pub origin: Origin,
    /// Maximum number of reported fingers.
    pub max_finger_count: u8,
    /// Maximum value of the touch-point width/height data.
    pub width_height_data_range: u8,
}

impl TouchMouseInfo {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            x_max_count: u16::from_be_bytes([payload[0], payload[1]]),
            y_max_count: u16::from_be_bytes([payload[2], payload[3]]),
            resolution_dpi: u16::from_be_bytes([payload[4], payload[5]]),
            origin: Origin::try_from(payload[6]).map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            max_finger_count: payload[7],
            width_height_data_range: payload[8],
        })
    }
}

/// Implements the `TouchMouseRaw` / `0x6110` feature.
pub struct TouchMouseRawFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<TouchMouseRawEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for TouchMouseRawFeature {
    const ID: u16 = 0x6110;
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

impl Feature for TouchMouseRawFeature {}

impl EmittingFeature<TouchMouseRawEvent> for TouchMouseRawFeature {
    fn listen(&self) -> async_channel::Receiver<TouchMouseRawEvent> {
        self.emitter.create_receiver()
    }
}

impl TouchMouseRawFeature {
    /// Retrieves the touch mouse's characteristics.
    pub async fn get_touchpad_info(&self) -> Result<TouchMouseInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        TouchMouseInfo::from_payload(&payload)
    }

    /// Retrieves the current raw-reporting mode.
    pub async fn get_raw_mode(&self) -> Result<RawMode, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        RawMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the raw-reporting mode.
    ///
    /// A raw mode must be selected for [`TouchMouseRawEvent::RawData`] events to
    /// be emitted.
    pub async fn set_raw_mode(&self, mode: RawMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [mode.into(), 0, 0]).await?;
        Ok(())
    }
}

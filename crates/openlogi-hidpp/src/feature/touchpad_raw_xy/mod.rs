//! Implements the `TouchpadRawXy` feature (ID `0x6100`) that exposes a
//! touchpad's raw multi-touch data: pad characteristics, the raw-report mode,
//! and a per-frame [`DualXyData`] event.

pub mod event;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use event::{DualXyData, TouchPoint, TouchpadRawEvent};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Raw-report mode flags from
    /// [`get_raw_report_state`](TouchpadRawXyFeature::get_raw_report_state).
    ///
    /// Some combinations are mutually exclusive; common valid bitmaps are `0x00`
    /// (off), `0x05`, `0x09`, `0x21` and `0x41` (see the feature spec).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct RawReportFlags: u8 {
        /// Raw reporting enabled.
        const RAW = 1 << 0;
        /// Add force data to 16-bit reporting (deprecated).
        const FORCE_ADD = 1 << 1;
        /// Enhanced reporting enabled.
        const ENHANCED = 1 << 2;
        /// Report width/height instead of area.
        const WIDTH_HEIGHT = 1 << 3;
        /// Report native gestures.
        const NATIVE_GESTURE = 1 << 4;
        /// Report major/minor/orientation.
        const MAJOR_MINOR = 1 << 5;
        /// Report 8-bit width and height bytes instead of area.
        const WIDTH_HEIGHT_8BIT = 1 << 6;
    }
}

/// The position of a touchpad's coordinate origin, viewed from above.
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

/// Touchpad characteristics from
/// [`get_touchpad_info`](TouchpadRawXyFeature::get_touchpad_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TouchpadInfo {
    /// Pad width in native coordinate units.
    pub x_size: u16,
    /// Pad height in native coordinate units.
    pub y_size: u16,
    /// Z-data range (`0x00` = none, `0x0f` = 16-bit).
    pub z_data_range: u8,
    /// Area-data range (`0x0f` = 16-bit).
    pub area_data_range: u8,
    /// Timestamp increment, in units of 0.1 ms.
    pub timestamp_units: u8,
    /// Maximum number of fingers that can be tracked.
    pub max_finger_count: u8,
    /// Position of the coordinate origin.
    pub origin: Origin,
    /// Whether pen input is supported.
    pub pen_support: bool,
    /// Raw-report mapping version.
    pub raw_report_mapping_version: u8,
    /// Native sensor DPI.
    pub dpi: u16,
}

impl TouchpadInfo {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            x_size: u16::from_be_bytes([payload[0], payload[1]]),
            y_size: u16::from_be_bytes([payload[2], payload[3]]),
            z_data_range: payload[4],
            area_data_range: payload[5],
            timestamp_units: payload[6],
            max_finger_count: payload[7],
            origin: Origin::try_from(payload[8]).map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            pen_support: payload[9] != 0,
            raw_report_mapping_version: payload[12],
            dpi: u16::from_be_bytes([payload[13], payload[14]]),
        })
    }
}

/// Implements the `TouchpadRawXy` / `0x6100` feature.
pub struct TouchpadRawXyFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<TouchpadRawEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for TouchpadRawXyFeature {
    const ID: u16 = 0x6100;
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

impl Feature for TouchpadRawXyFeature {}

impl EmittingFeature<TouchpadRawEvent> for TouchpadRawXyFeature {
    fn listen(&self) -> async_channel::Receiver<TouchpadRawEvent> {
        self.emitter.create_receiver()
    }
}

impl TouchpadRawXyFeature {
    /// Retrieves the touchpad's characteristics.
    pub async fn get_touchpad_info(&self) -> Result<TouchpadInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        TouchpadInfo::from_payload(&payload)
    }

    /// Retrieves the current raw-report mode.
    pub async fn get_raw_report_state(&self) -> Result<RawReportFlags, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(RawReportFlags::from_bits_retain(payload[0]))
    }

    /// Sets the raw-report mode.
    ///
    /// Enable [`RawReportFlags::RAW`] for [`TouchpadRawEvent`]s to be emitted.
    pub async fn set_raw_report_state(&self, flags: RawReportFlags) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [flags.bits(), 0, 0]).await?;
        Ok(())
    }
}

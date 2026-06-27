//! Implements the `ExtendedAdjustableDpi` feature (ID `0x2202`).
//!
//! This is the modern successor to [`AdjustableDpi`](super::adjustable_dpi)
//! (`0x2201`). On top of a single per-sensor DPI it adds independent X/Y DPI,
//! lift-off distance, DPI-status LED control and a DPI calibration flow, and it
//! describes the supported DPI as fixed values and stepped ranges rather than a
//! flat list.

pub mod event;
pub mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use event::{DpiCalibrationCompleted, DpiParametersChanged, ExtendedDpiEvent};
pub use types::{
    CalibrationType, DpiCalibrationCorrection, DpiCalibrationInfo, DpiDirection, DpiParameters,
    DpiRange, LedHoldType, Lod, SensorCapabilities, SensorCapabilitiesInfo, SetDpiParameters,
    ShowDpiStatus, StartDpiCalibration,
};

use self::types::{parse_dpi_list, parse_dpi_ranges, parse_lod_list, terminated_word_len};
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

/// Upper bound on the number of `getSensorDpiRanges` pages fetched before the
/// device is considered to be returning a malformed, unterminated list.
const MAX_RANGE_PAGES: u8 = 16;

/// Implements the `ExtendedAdjustableDpi` / `0x2202` feature.
pub struct ExtendedDpiFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<ExtendedDpiEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for ExtendedDpiFeature {
    const ID: u16 = 0x2202;
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

impl Feature for ExtendedDpiFeature {}

impl EmittingFeature<ExtendedDpiEvent> for ExtendedDpiFeature {
    fn listen(&self) -> async_channel::Receiver<ExtendedDpiEvent> {
        self.emitter.create_receiver()
    }
}

impl ExtendedDpiFeature {
    /// Retrieves the number of motion sensors the device exposes.
    pub async fn get_sensor_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Retrieves the capabilities and DPI-level count of `sensor_index`.
    pub async fn get_sensor_capabilities(
        &self,
        sensor_index: u8,
    ) -> Result<SensorCapabilitiesInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [sensor_index, 0, 0])
            .await?
            .extend_payload();
        Ok(SensorCapabilitiesInfo {
            sensor_index: payload[0],
            dpi_level_count: payload[1],
            capabilities: SensorCapabilities::from_bits_retain(payload[2]),
        })
    }

    /// Retrieves the supported DPI of `sensor_index` along `direction` as a mix
    /// of fixed values and stepped ranges.
    ///
    /// The device may split the description across several pages; this fetches
    /// them until the `0x0000` end-of-list terminator is seen, then decodes the
    /// accumulated stream. A device that never terminates the list within a
    /// bounded number of pages yields [`Hidpp20Error::UnsupportedResponse`].
    pub async fn get_sensor_dpi_ranges(
        &self,
        sensor_index: u8,
        direction: DpiDirection,
    ) -> Result<Vec<DpiRange>, Hidpp20Error> {
        let mut stream = Vec::new();
        for page in 0..MAX_RANGE_PAGES {
            let payload = self
                .endpoint
                .call(2, [sensor_index, direction.into(), page])
                .await?
                .extend_payload();
            // Validate the echoed addressing (sensor, direction, page) before
            // trusting the page body, so a mismatched page cannot corrupt the
            // accumulated stream.
            if payload[0] != sensor_index || payload[1] != u8::from(direction) || payload[2] != page
            {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            stream.extend_from_slice(&payload[3..16]);
            if terminated_word_len(&stream).is_some() {
                return parse_dpi_ranges(&stream);
            }
        }
        Err(Hidpp20Error::UnsupportedResponse)
    }

    /// Retrieves the current profile's DPI list for `sensor_index` along
    /// `direction`.
    ///
    /// Only meaningful when the sensor supports profiles
    /// ([`SensorCapabilities::PROFILE`]); otherwise the device returns an error.
    pub async fn get_sensor_dpi_list(
        &self,
        sensor_index: u8,
        direction: DpiDirection,
    ) -> Result<Vec<u16>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(3, [sensor_index, direction.into(), 0])
            .await?
            .extend_payload();
        // Skip the echoed sensor index and direction in bytes 0 and 1.
        parse_dpi_list(&payload[2..])
    }

    /// Retrieves the current profile's lift-off-distance list for
    /// `sensor_index`.
    ///
    /// The list length is the sensor's DPI-level count
    /// ([`SensorCapabilitiesInfo::dpi_level_count`]), which the caller passes as
    /// `dpi_level_count`; the device does not delimit the list.
    pub async fn get_sensor_lod_list(
        &self,
        sensor_index: u8,
        dpi_level_count: u8,
    ) -> Result<Vec<Lod>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(4, [sensor_index, 0, 0])
            .await?
            .extend_payload();
        // Skip the echoed sensor index in byte 0.
        parse_lod_list(&payload[1..], usize::from(dpi_level_count))
    }

    /// Retrieves the current and default DPI parameters of `sensor_index`.
    pub async fn get_sensor_dpi_parameters(
        &self,
        sensor_index: u8,
    ) -> Result<DpiParameters, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(5, [sensor_index, 0, 0])
            .await?
            .extend_payload();
        Ok(DpiParameters {
            sensor_index: payload[0],
            dpi_x: u16::from_be_bytes([payload[1], payload[2]]),
            default_dpi_x: u16::from_be_bytes([payload[3], payload[4]]),
            dpi_y: u16::from_be_bytes([payload[5], payload[6]]),
            default_dpi_y: u16::from_be_bytes([payload[7], payload[8]]),
            lod: Lod::try_from(payload[9]).map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }

    /// Sets the DPI and lift-off distance of `sensor_index`.
    ///
    /// `params.dpi_y` must be `0` when the sensor has no independent Y axis.
    pub async fn set_sensor_dpi_parameters(
        &self,
        sensor_index: u8,
        params: SetDpiParameters,
    ) -> Result<(), Hidpp20Error> {
        let [dpi_x_hi, dpi_x_lo] = params.dpi_x.to_be_bytes();
        let [dpi_y_hi, dpi_y_lo] = params.dpi_y.to_be_bytes();
        let mut args = [0; 16];
        args[..6].copy_from_slice(&[
            sensor_index,
            dpi_x_hi,
            dpi_x_lo,
            dpi_y_hi,
            dpi_y_lo,
            params.lod.into(),
        ]);
        self.endpoint.call_long(6, args).await?;
        Ok(())
    }

    /// Asks the device to show `params.dpi_level` on its DPI status LED.
    ///
    /// Valid only while the device is in host mode.
    pub async fn show_sensor_dpi_status(
        &self,
        sensor_index: u8,
        params: ShowDpiStatus,
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[..4].copy_from_slice(&[
            sensor_index,
            params.dpi_level,
            params.led_hold_type.into(),
            params.button_num,
        ]);
        self.endpoint.call_long(7, args).await?;
        Ok(())
    }

    /// Retrieves the reference information needed to start a calibration of
    /// `sensor_index`.
    pub async fn get_dpi_calibration_info(
        &self,
        sensor_index: u8,
    ) -> Result<DpiCalibrationInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(8, [sensor_index, 0, 0])
            .await?
            .extend_payload();
        Ok(DpiCalibrationInfo {
            sensor_index: payload[0],
            mouse_width: payload[1],
            mouse_length: u16::from_be_bytes([payload[2], payload[3]]),
            calib_dpi_x: u16::from_be_bytes([payload[4], payload[5]]),
            calib_dpi_y: u16::from_be_bytes([payload[6], payload[7]]),
        })
    }

    /// Starts a DPI calibration of `sensor_index`.
    ///
    /// Requires [`SensorCapabilities::CALIBRATION`]. The device reports the
    /// outcome through an [`ExtendedDpiEvent::CalibrationCompleted`] event; for a
    /// [`CalibrationType::Software`] calibration the result is then applied with
    /// [`Self::set_dpi_calibration`].
    pub async fn start_dpi_calibration(
        &self,
        sensor_index: u8,
        params: StartDpiCalibration,
    ) -> Result<(), Hidpp20Error> {
        let [count_hi, count_lo] = params.expected_count.to_be_bytes();
        let mut args = [0; 16];
        args[..8].copy_from_slice(&[
            sensor_index,
            params.direction.into(),
            count_hi,
            count_lo,
            params.calib_type.into(),
            params.start_timeout,
            params.hw_process_timeout,
            params.sw_process_timeout,
        ]);
        self.endpoint.call_long(9, args).await?;
        Ok(())
    }

    /// Applies a calibration correction to `sensor_index` along `direction`.
    ///
    /// Allowed only while a calibration started by [`Self::start_dpi_calibration`]
    /// is in progress (or to revert, see [`DpiCalibrationCorrection`]).
    pub async fn set_dpi_calibration(
        &self,
        sensor_index: u8,
        direction: DpiDirection,
        correction: DpiCalibrationCorrection,
    ) -> Result<(), Hidpp20Error> {
        let [cor_hi, cor_lo] = correction.to_wire().to_be_bytes();
        let mut args = [0; 16];
        args[..4].copy_from_slice(&[sensor_index, direction.into(), cor_hi, cor_lo]);
        self.endpoint.call_long(10, args).await?;
        Ok(())
    }
}

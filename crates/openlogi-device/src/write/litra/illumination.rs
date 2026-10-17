//! Litra lights over HID++ `0x1990 Illumination`, the protocol their raw
//! reports already speak.
//!
//! The fixed report in [`super::encode_command`] is a HID++ 2.0 long report
//! to device `0xff`, feature index 4: functions 1, 4 and 9 are
//! `setIllumination`, `setBrightness` and `setColorTemperature`. Logi Options+
//! resolves the index through the root feature instead, reads the light's
//! ranges and state, and only writes a value that differs from the one it
//! read (reverse-engineered from `feature_x1990_illumination_light::start`
//! and its setters). This module does the same on the Litra's own HID node, so
//! a firmware that moves the feature still works and every write is read
//! back. [`super::apply`] falls back to the fixed report when the light does
//! not answer here.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::{Device, DeviceError},
    feature::EmittingFeature,
    feature::illumination::{
        ControlCapabilities, ControlInfo, IlluminationEvent, IlluminationFeature, IlluminationState,
    },
    protocol::v20::Hidpp20Error,
};
use openlogi_core::device::{LightCapabilities, LightState, LightValueRange, LightValueUnit};

use crate::channel::route::DIRECT_DEVICE_INDEX;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature_with_version};

use super::{LightCommand, percent_to_native, unsupported};

/// How long the root ping, the `0x1990` lookup and the first state read may
/// take before the light is taken to be one that only accepts the fixed raw
/// report.
///
/// A light that answers HID++ does all of it in milliseconds; one that does
/// not would otherwise hold the command for the channel's five-second
/// request timeout, once per request.
pub(super) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(1);

/// The HID++ `Illumination` feature id.
const ILLUMINATION: u16 = 0x1990;

/// Why a light's `0x1990` could not be reached over HID++.
pub(super) enum Unresolved {
    /// The root ping failed: no HID++ 2.0 device answers at `0xff`.
    Ping(DeviceError),
    /// The root lookup failed, or the light lists no `0x1990`.
    Lookup(WriteError),
    /// The light lists `0x1990` but reading its state failed.
    Read(WriteError),
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping(error) => write!(f, "root ping failed: {error:?}"),
            Self::Lookup(error) => write!(f, "0x1990 lookup failed: {error}"),
            Self::Read(error) => write!(f, "0x1990 state read failed: {error}"),
        }
    }
}

/// A Litra's `0x1990` feature, resolved through its root feature.
pub(super) struct Illumination {
    feature: Arc<IlluminationFeature>,
    /// The feature version the root lookup reported.
    version: u8,
}

/// Resolve `0x1990` on the light behind `channel` and read its state.
///
/// The read belongs here, not to the command: the raw report is the path
/// proven on hardware, so a light that lists `0x1990` but cannot be read over
/// it is better served by that report than by an error.
pub(super) async fn resolve(
    channel: Arc<HidppChannel>,
) -> Result<(Illumination, LightState), Unresolved> {
    let mut device = Device::new(channel, DIRECT_DEVICE_INDEX)
        .await
        .map_err(Unresolved::Ping)?;
    let (feature, version) = open_feature_with_version::<IlluminationFeature>(&mut device)
        .await
        .map_err(Unresolved::Lookup)?;
    let light = Illumination { feature, version };
    let state = light.read_state().await.map_err(Unresolved::Read)?;
    Ok((light, state))
}

fn hidpp_failed(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Light, ILLUMINATION)
}

/// The range a control's info describes, or `None` when the control has a
/// single setting — the specification leaves its setter unsupported then.
///
/// The top is lowered onto the resolution grid, so every value inside the
/// range is one the light accepts. A zero resolution is read as 1.
pub(super) fn control_range(
    min: u16,
    max: u16,
    resolution: u16,
    unit: LightValueUnit,
) -> Option<LightValueRange> {
    if min >= max {
        return None;
    }
    let step = resolution.max(1);
    let max = max - (max - min) % step;
    LightValueRange::new(min, max, step, unit).ok()
}

fn info_range(info: ControlInfo, unit: LightValueUnit) -> Option<LightValueRange> {
    control_range(info.min, info.max, info.resolution, unit)
}

impl Illumination {
    /// The light's notifications, from now on.
    pub(super) fn listen(
        &self,
    ) -> impl futures_lite::Stream<Item = IlluminationEvent> + Send + 'static {
        self.feature.listen()
    }

    /// Read the light's ranges and current state, in the order Logi Options+
    /// reads them on start.
    pub(super) async fn read_state(&self) -> Result<LightState, WriteError> {
        let brightness_info = self
            .feature
            .get_brightness_info()
            .await
            .map_err(hidpp_failed)?;
        let brightness_range = info_range(brightness_info, LightValueUnit::Lumens);
        let brightness_limit = match brightness_range {
            Some(_) => self.brightness_limit(brightness_info).await?,
            None => None,
        };
        let temperature_range = info_range(
            self.feature
                .get_color_temperature_info()
                .await
                .map_err(hidpp_failed)?,
            LightValueUnit::Kelvin,
        );
        let on = self.is_on().await?;
        let brightness = match brightness_range {
            Some(_) => Some(self.feature.get_brightness().await.map_err(hidpp_failed)?),
            None => None,
        };
        let temperature_kelvin = match temperature_range {
            Some(_) => Some(
                self.feature
                    .get_color_temperature()
                    .await
                    .map_err(hidpp_failed)?,
            ),
            None => None,
        };
        Ok(LightState {
            capabilities: LightCapabilities {
                power: true,
                brightness: brightness_range,
                temperature: temperature_range,
                color: false,
                zones: false,
            },
            on,
            brightness,
            temperature_kelvin,
            brightness_limit,
        })
    }

    /// The brightness limit in effect, if one lowers the top of the range.
    ///
    /// `getBrightnessEffectiveMax` is a version-1 function, and only a light
    /// whose brightness info flags a dynamic maximum has one — Logi Options+
    /// reads it under the same flag. `0` means no limit is in effect.
    async fn brightness_limit(&self, info: ControlInfo) -> Result<Option<u16>, WriteError> {
        if self.version < 1
            || !info
                .capabilities
                .contains(ControlCapabilities::HAS_DYNAMIC_MAXIMUM)
        {
            return Ok(None);
        }
        let limit = self
            .feature
            .get_brightness_effective_max()
            .await
            .map_err(hidpp_failed)?;
        Ok((limit != 0 && limit < info.max).then_some(limit))
    }

    async fn is_on(&self) -> Result<bool, WriteError> {
        let state = self
            .feature
            .get_illumination()
            .await
            .map_err(hidpp_failed)?;
        Ok(state == IlluminationState::On)
    }

    /// Apply `command` to a light last read as `state`, and return the state
    /// it reports afterwards.
    ///
    /// The value is checked against the ranges the light itself reports, and
    /// written only when it differs from the current one. The control written
    /// is read back, so a brightness the light lowered to its limit comes
    /// back as the value it kept.
    pub(super) async fn apply(
        &self,
        mut state: LightState,
        command: LightCommand,
    ) -> Result<LightState, WriteError> {
        match command {
            LightCommand::Power(on) => {
                if state.on != on {
                    self.feature
                        .set_illumination(IlluminationState::from(on))
                        .await
                        .map_err(hidpp_failed)?;
                    state.on = self.is_on().await?;
                }
            }
            LightCommand::BrightnessPercent(percent) => {
                let range = state
                    .capabilities
                    .brightness
                    .ok_or_else(|| unsupported("brightness"))?;
                let lumens = percent_to_native(percent, range)?;
                self.set_brightness(&mut state, lumens).await?;
            }
            LightCommand::BrightnessNative(value) => {
                let range = state
                    .capabilities
                    .brightness
                    .ok_or_else(|| unsupported("brightness"))?;
                if !range.contains(value) {
                    return Err(WriteError::InvalidLightValue {
                        control: "brightness_native".into(),
                        value,
                    });
                }
                self.set_brightness(&mut state, value).await?;
            }
            LightCommand::TemperatureKelvin(kelvin) => {
                let range = state
                    .capabilities
                    .temperature
                    .ok_or_else(|| unsupported("temperature"))?;
                if !range.contains(kelvin) {
                    return Err(WriteError::InvalidLightValue {
                        control: "temperature_kelvin".into(),
                        value: kelvin,
                    });
                }
                if state.temperature_kelvin != Some(kelvin) {
                    self.feature
                        .set_color_temperature(kelvin)
                        .await
                        .map_err(hidpp_failed)?;
                    state.temperature_kelvin = Some(
                        self.feature
                            .get_color_temperature()
                            .await
                            .map_err(hidpp_failed)?,
                    );
                }
            }
        }
        Ok(state)
    }

    async fn set_brightness(&self, state: &mut LightState, lumens: u16) -> Result<(), WriteError> {
        if state.brightness == Some(lumens) {
            return Ok(());
        }
        self.feature
            .set_brightness(lumens)
            .await
            .map_err(hidpp_failed)?;
        state.brightness = Some(self.feature.get_brightness().await.map_err(hidpp_failed)?);
        Ok(())
    }
}

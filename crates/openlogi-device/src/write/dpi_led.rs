//! HID++ `0x1300 LedControl` writes: the DPI LED that precision mode lights.
//!
//! Logi Options+ lights the DPI LED of a mouse in precision mode and darkens
//! it when the mode ends (`mouse_manager::_toggle_precision_mode`). It
//! drives the first LED of type DPI. It takes software control of the LEDs
//! first, when the mouse does not grant it yet, and never hands control back
//! to the firmware. [`set_dpi_led_on`] does the same.

use hidpp::{
    device::Device,
    feature::CreatableFeature,
    feature::led_control::{LedControlFeature, LedKind, LedState},
    protocol::v20::Hidpp20Error,
};
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature};
use crate::SharedChannel;

/// What [`set_dpi_led_on`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiLed {
    /// The DPI LED now shows the requested state.
    Set,
    /// The mouse has no `0x1300` LED of type DPI, so nothing was written.
    Missing,
}

/// Light (`on`) or darken the DPI LED of the mouse on an already-open
/// [`SharedChannel`].
///
/// Returns [`DpiLed::Missing`] when the mouse has no `0x1300` feature or no
/// LED of type DPI.
pub async fn set_dpi_led_on(shared: &SharedChannel, on: bool) -> Result<DpiLed, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(std::sync::Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let leds = match open_feature::<LedControlFeature>(&mut device).await {
        Ok(leds) => leds,
        Err(WriteError::FeatureUnsupported { .. }) => return Ok(DpiLed::Missing),
        Err(error) => return Err(error),
    };
    let Some(led) = dpi_led(&leds).await.map_err(led_error)? else {
        return Ok(DpiLed::Missing);
    };
    if !leds.get_sw_control().await.map_err(led_error)? {
        leds.set_sw_control(true).await.map_err(led_error)?;
    }
    let state = if on { LedState::On } else { LedState::Off };
    leds.set_led_state(led, state).await.map_err(led_error)?;
    debug!(index, led, on, "DPI LED written");
    Ok(DpiLed::Set)
}

/// The index of the first LED of type DPI, as Logi Options+ picks it.
async fn dpi_led(leds: &LedControlFeature) -> Result<Option<u8>, Hidpp20Error> {
    for led in 0..leds.get_led_count().await? {
        if leds.get_led_info(led).await?.kind == LedKind::Dpi {
            return Ok(Some(led));
        }
    }
    Ok(None)
}

fn led_error(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::WriteLed, LedControlFeature::ID)
}

#[cfg(test)]
mod tests;

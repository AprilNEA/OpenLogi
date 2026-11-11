//! A keypad's LCD brightness over HID++ `0x8040` BrightnessControl.
//!
//! The MX Creative Keypad reports a range of `0..=100` in 101 steps and no
//! illumination switch (hardware read 2026-11-03, `docs/hardware/
//! mx-creative-console.md`). The setting is a percentage of whatever range
//! the device reports, and never maps to the bottom of it: a reference driver
//! says writing `0` resets the device, which has not been verified, so that
//! value is not offered.

use std::sync::Arc;

use hidpp::device::Device;
use hidpp::feature::brightness_control::BrightnessControlFeature;
use openlogi_core::config::KeyDisplayBrightness;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature};
use crate::SharedChannel;

/// Write `brightness` to the keypad behind `shared` and return the raw level
/// written.
pub async fn set_key_display_brightness_on(
    shared: &SharedChannel,
    brightness: KeyDisplayBrightness,
) -> Result<u16, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<BrightnessControlFeature>(&mut device).await?;
    let classify = |e| classify_hidpp_error(e, HidppOperation::WriteKeyBrightness, 0x8040);
    let info = feature.get_info().await.map_err(classify)?;
    let level = brightness.scale(info.min_brightness, info.max_brightness);
    feature.set_brightness(level).await.map_err(classify)?;
    Ok(level)
}

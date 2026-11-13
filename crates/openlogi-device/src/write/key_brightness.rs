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

/// Read the brightness the keypad behind `shared` shows now, as a
/// percentage of the range it reports ([`KeyDisplayBrightness::from_level`]).
pub async fn get_key_display_brightness_on(
    shared: &SharedChannel,
) -> Result<KeyDisplayBrightness, WriteError> {
    let feature = open_brightness(shared).await?;
    let classify = |e| classify_hidpp_error(e, HidppOperation::ReadKeyBrightness, 0x8040);
    let info = feature.get_info().await.map_err(classify)?;
    let level = feature.get_brightness().await.map_err(classify)?;
    Ok(KeyDisplayBrightness::from_level(
        level,
        info.min_brightness,
        info.max_brightness,
    ))
}

/// Write `brightness` to the keypad behind `shared` and answer with the
/// brightness it reads back afterwards, which need not be `brightness` on a
/// panel coarser than one percent.
pub async fn set_key_display_brightness_on(
    shared: &SharedChannel,
    brightness: KeyDisplayBrightness,
) -> Result<KeyDisplayBrightness, WriteError> {
    let feature = open_brightness(shared).await?;
    let classify = |e| classify_hidpp_error(e, HidppOperation::WriteKeyBrightness, 0x8040);
    let info = feature.get_info().await.map_err(classify)?;
    let level = brightness.scale(info.min_brightness, info.max_brightness);
    feature.set_brightness(level).await.map_err(classify)?;
    let read = feature.get_brightness().await.map_err(classify)?;
    Ok(KeyDisplayBrightness::from_level(
        read,
        info.min_brightness,
        info.max_brightness,
    ))
}

/// The `0x8040` feature of the device behind `shared`.
async fn open_brightness(
    shared: &SharedChannel,
) -> Result<Arc<BrightnessControlFeature>, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<BrightnessControlFeature>(&mut device).await
}

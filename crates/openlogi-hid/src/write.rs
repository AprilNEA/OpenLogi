//! HID++ writes back to the device — DPI, SmartShift, lighting, backlight, and
//! diagnostics.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through `open_route_channel`, so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! route-addressed call re-enumerates and re-opens, while the corresponding
//! `_on` entry points reuse a [`SharedChannel`] already owned by inventory or a
//! standalone capture session.

use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device, feature::CreatableFeature};
use openlogi_core::config::LightSettings;
use openlogi_core::device::LightCapabilities;

use crate::route::{DeviceRoute, open_route_channel};

mod backlight;
mod diagnostics;
mod dpi;
mod error;
mod fn_lock;
mod haptic;
mod lighting;
mod litra;
mod shared;
mod smartshift;

pub use backlight::{get_backlight, set_backlight_enabled};
pub use diagnostics::{
    FeatureEntry, ReprogControlEntry, dump_features, dump_reprog_controls, read_battery_raw,
};
pub use dpi::{DpiCapabilities, DpiInfo, get_dpi, get_dpi_info, set_dpi};
pub use error::{HidppFeatureErrorKind, HidppOperation, WriteError};
pub use fn_lock::set_fn_lock;
pub use haptic::{play_haptic, play_haptic_on};
pub use hidpp::feature::haptic_feedback::HapticWaveform;
pub use lighting::{LightingMethod, set_keyboard_color, set_keyboard_color_with};
pub use litra::{
    LightCommand, LitraModel, apply as apply_litra, encode_command as encode_litra_command,
    matches_litra,
};
pub use shared::{
    SharedChannel, get_dpi_info_on, get_smartshift_status_on, set_dpi_on, set_fn_lock_on,
    set_keyboard_color_on, set_keyboard_color_with_on, set_smartshift_on, toggle_smartshift_on,
};
pub use smartshift::{
    get_smartshift_status, set_smartshift, set_smartshift_sensitivity, toggle_smartshift,
};

/// Expand protocol-neutral saved settings into only the controls advertised
/// by a standalone light. Unsupported controls are omitted rather than sent
/// speculatively, which keeps power-only and brightness-only drivers usable.
#[must_use]
pub fn commands_for_light_settings(
    settings: LightSettings,
    capabilities: LightCapabilities,
) -> Vec<LightCommand> {
    let mut commands = Vec::new();
    if capabilities.power {
        commands.push(LightCommand::Power(settings.enabled));
    }
    if capabilities.brightness.is_some() {
        commands.push(LightCommand::BrightnessPercent(settings.brightness_percent));
    }
    if capabilities.temperature.is_some()
        && let Some(kelvin) = settings.temperature_kelvin
    {
        commands.push(LightCommand::TemperatureKelvin(kelvin));
    }
    commands
}

pub(crate) use error::classify_hidpp_error;

/// Look up `F` on a device by HID++ feature ID, register it with
/// [`Device::add_feature`], and return the typed wrapper.
///
/// The direct lookup via `root().get_feature(id)` returns the assigned index
/// unconditionally; `add_feature` then attaches our wrapper to that index. This
/// keeps route-based write/read paths independent from full feature-table
/// enumeration and also works for feature wrappers that are not in the central
/// registry yet.
pub(crate) async fn open_feature<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<Arc<F>, WriteError> {
    let info = device
        .root()
        .get_feature(F::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, F::ID))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: F::ID })?;
    Ok(device.add_feature::<F>(info.index))
}

/// Boilerplate-eater: open the channel that reaches `route`, then run `f` once
/// with it. The caller addresses features at [`DeviceRoute::device_index`].
pub(crate) async fn with_route<F, Fut, T>(route: &DeviceRoute, f: F) -> Result<T, WriteError>
where
    F: FnOnce(Arc<HidppChannel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, WriteError>>,
{
    match open_route_channel(route).await? {
        Some(channel) => f(channel).await,
        None => Err(WriteError::DeviceNotFound),
    }
}

#[cfg(test)]
mod tests;

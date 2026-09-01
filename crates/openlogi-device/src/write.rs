//! HID++ reads and writes per feature — DPI, SmartShift, wheel modes,
//! lighting, backlight, and diagnostics.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through `open_route_channel`, so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! route-addressed call re-enumerates and re-opens, while the corresponding
//! `_on` entry points reuse a [`crate::SharedChannel`] already owned by
//! inventory or a standalone capture session.

use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device, feature::CreatableFeature};

use crate::backend::HidBackend;
use crate::channel::route::{DeviceRoute, open_route_channel};

mod backlight;
mod diagnostics;
mod dpi;
mod error;
mod fn_lock;
mod haptic;
mod hires_wheel;
mod lighting;
mod litra;
mod smartshift;

pub use backlight::{get_backlight, set_backlight_enabled};
pub use diagnostics::{
    FeatureEntry, FirmwareEntity, FirmwareEntityInfo, ReprogControlEntry, dump_features,
    dump_firmware_entities, dump_reprog_controls, read_battery_raw,
};
pub use dpi::{
    Dpi, DpiCapabilities, DpiInfo, get_dpi, get_dpi_info, get_dpi_info_on, set_dpi, set_dpi_on,
};
pub use error::{HidppFeatureErrorKind, HidppOperation, WriteError};
pub use fn_lock::{set_fn_lock, set_fn_lock_on};
pub use haptic::{ensure_haptics_armed_on, play_haptic, play_haptic_on};
pub use hidpp::feature::haptic_feedback::HapticWaveform;
pub use hires_wheel::{
    ScrollReportingTarget, ScrollResolution, ScrollWheelMode, get_scroll_wheel_mode,
    get_scroll_wheel_mode_on, set_scroll_inversion, set_scroll_inversion_on, set_scroll_resolution,
    set_scroll_resolution_on, set_scroll_wheel_mode, set_scroll_wheel_mode_on,
};
pub use lighting::{
    LightingMethod, set_keyboard_color, set_keyboard_color_on, set_keyboard_color_with,
    set_keyboard_color_with_on,
};
pub(crate) use litra::litra_capabilities;
pub use litra::{
    LITRA_BEAM_PRODUCT_ID, LITRA_GLOW_PRODUCT_ID, LightCommand, LitraDescriptor, LitraModel,
    apply as apply_litra, encode_command as encode_litra_command, find_litra,
    litra_model_for_route, matches_litra,
};
pub use smartshift::{
    get_smartshift_status, get_smartshift_status_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, toggle_smartshift, toggle_smartshift_on,
};

// commands_for_light_settings operates purely on openlogi_core config/device
// types with no HID++ I/O, so it lives in `openlogi_core::hid::light`;
// re-exported here unchanged so this module's own API surface doesn't churn.
pub use openlogi_core::hid::light::commands_for_light_settings;

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
pub(crate) async fn with_route<F, Fut, T>(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    f: F,
) -> Result<T, WriteError>
where
    F: FnOnce(Arc<HidppChannel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, WriteError>>,
{
    match open_route_channel(backend, route).await? {
        Some(channel) => f(channel).await,
        None => Err(WriteError::DeviceNotFound),
    }
}

#[cfg(test)]
mod tests;

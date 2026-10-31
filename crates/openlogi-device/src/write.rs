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

use hidpp::{
    channel::{ChannelError, HidppChannel},
    device::Device,
    feature::{CreatableFeature, disable_keys::DisableKeysFeature},
};

use crate::backend::HidBackend;
use crate::channel::route::{DeviceRoute, open_route_channel};

mod backlight;
mod button_spy;
mod diagnostics;
mod disable_keys;
mod dpi;
mod dpi_led;
mod easy_switch;
mod error;
mod fn_lock;
mod force_sensing;
mod haptic;
mod hires_wheel;
mod key_display;
mod lighting;
mod litra;
mod onboard;
mod os_layout;
mod pointer_scaling;
mod report_rate;
mod smartshift;

pub use backlight::{get_backlight, get_backlight_on, update_backlight, update_backlight_on};
pub use button_spy::{ButtonSpyError, ButtonSpyWatch, watch_button_spy};
pub use diagnostics::{
    FeatureEntry, FirmwareEntity, FirmwareEntityInfo, RAW_BATTERY_FEATURES, ReprogControlEntry,
    dump_features, dump_firmware_entities, dump_reprog_controls, read_battery_raw,
};
pub use disable_keys::{
    get_disable_keys, get_disable_keys_on, set_disable_keys, set_disable_keys_on,
    set_disable_keys_write_through_on,
};
pub use dpi::{
    Dpi, DpiCapabilities, DpiInfo, DpiTarget, get_dpi, get_dpi_info, get_dpi_info_on, set_dpi,
    set_dpi_on, set_dpi_target_on,
};
pub use dpi_led::{DpiLed, set_dpi_led_on};
pub(crate) use easy_switch::read_name as read_host_friendly_name;
pub use easy_switch::{
    get_easy_switch_hosts, get_easy_switch_hosts_on, rename_easy_switch_host,
    rename_easy_switch_host_on,
};
pub use error::{HidppFeatureErrorKind, HidppOperation, WriteError};
pub use fn_lock::{get_fn_lock, get_fn_lock_on, set_fn_lock, set_fn_lock_on};
pub use force_sensing::{get_force_sensing_on, set_force_sensing_on};
pub use haptic::{
    HapticRestore, HapticRestoreOutcome, ensure_haptics_armed_on, play_haptic, play_haptic_on,
    restore_haptics_on,
};
pub use hidpp::feature::haptic_feedback::HapticWaveform;
pub use hidpp::feature::multi_platform::OsMask;
pub use hires_wheel::{
    ScrollReportingTarget, ScrollResolution, ScrollWheelMode, get_scroll_wheel_mode,
    get_scroll_wheel_mode_on, set_scroll_inversion, set_scroll_inversion_on, set_scroll_resolution,
    set_scroll_resolution_on, set_scroll_wheel_mode, set_scroll_wheel_mode_on,
};
pub use key_display::{KeyDisplay, KeyImageJpeg, key_display_for, write_key_image_on};
pub use lighting::{
    LightingMethod, LightingWrite, RgbControlLease, release_rgb_control, set_keyboard_color,
    set_keyboard_color_on, set_keyboard_color_with, set_keyboard_color_with_on,
};
pub(crate) use litra::litra_capabilities;
pub use litra::{
    LITRA_BEAM_PRODUCT_ID, LITRA_GLOW_PRODUCT_ID, LightChange, LightCommand, LightControls,
    LightWatch, LitraDescriptor, LitraModel, apply as apply_litra,
    encode_command as encode_litra_command, find_litra, litra_model_for_route, matches_litra,
    read_state as read_litra_state, watch as watch_litra,
};
pub use onboard::{get_onboard, get_onboard_on};
pub use openlogi_core::hid::{DisableKeysMask, DisableKeysState};
pub use openlogi_core::hid::{
    EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind,
};
pub use openlogi_core::hid::{ForceSensingInfo, PointerSpeed, ReportRate, ReportRateInfo};
pub use os_layout::{HostOs, OsLayoutOutcome, OsVersion, keep_os_layout_on};
pub use pointer_scaling::{
    PointerScaling, get_pointer_scaling, get_pointer_speed_on, set_pointer_scaling,
    set_pointer_speed_on,
};
pub(crate) use report_rate::set_report_rate_on_channel;
pub use report_rate::{
    get_report_rate_info, get_report_rate_info_on, set_report_rate, set_report_rate_on,
};
pub use smartshift::{
    get_smartshift_status, get_smartshift_status_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, set_smartshift_torque, toggle_smartshift, toggle_smartshift_on,
};

// commands_for_light_settings operates purely on openlogi_core config/device
// types with no HID++ I/O, so it lives in `openlogi_core::hid::light`; this
// module's callers name it through here, next to the writes it feeds.
pub use openlogi_core::hid::light::commands_for_light_settings;

pub(crate) use error::classify_hidpp_error;
pub(crate) use force_sensing::{force_button_is_changeable, panel_button};

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
    open_feature_with_version::<F>(device)
        .await
        .map(|(feature, _)| feature)
}

/// [`open_feature`], also returning the feature version the device reports
/// in the same root `getFeature` reply.
pub(crate) async fn open_feature_with_version<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<(Arc<F>, u8), WriteError> {
    let info = device
        .root()
        .get_feature(F::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, F::ID))?;
    register_feature::<F>(device, info)
}

/// Eligibility and wrapper registration have one owner for both I/O lifetimes.
fn register_feature<F: CreatableFeature + 'static>(
    device: &mut Device,
    info: Option<hidpp::feature::root::FeatureInformation>,
) -> Result<(Arc<F>, u8), WriteError> {
    let info = info.ok_or(WriteError::FeatureUnsupported { feature_hex: F::ID })?;
    if F::ID == DisableKeysFeature::ID && !info.typ.is_end_user_configurable() {
        return Err(WriteError::FeatureUnsupported { feature_hex: F::ID });
    }
    Ok((device.add_feature::<F>(info.index), info.version))
}

async fn open_feature_write_through<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<Arc<F>, WriteError> {
    let info = device
        .root()
        .get_feature_write_through(F::ID)
        .await
        .map_err(|error| classify_hidpp_error(error, HidppOperation::ResolveFeature, F::ID))?;
    register_feature::<F>(device, info).map(|(feature, _)| feature)
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

/// Write `report` whole through `channel`'s raw-report path, for the report
/// widths an HID++ message cannot carry — the per-key lighting frame and the
/// keypad's image write.
///
/// A transport timeout is `operation`'s [`WriteError::RequestTimedOut`]. A
/// short write is an error too: the device acts only on a complete report.
pub(crate) async fn write_whole_raw_report(
    channel: &HidppChannel,
    report: &[u8],
    operation: HidppOperation,
) -> Result<(), WriteError> {
    let written = channel
        .write_raw_report(report)
        .await
        .map_err(|error| match error {
            ChannelError::Timeout => WriteError::RequestTimedOut { operation },
            other => WriteError::Hidpp(format!("{other:?}")),
        })?;
    if written == report.len() {
        Ok(())
    } else {
        Err(WriteError::Hidpp(format!(
            "{operation:?} report wrote {written} of {} bytes",
            report.len()
        )))
    }
}

#[cfg(test)]
mod tests;

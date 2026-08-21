//! The route-addressed device API, wired to this host's HID stack.
//!
//! Resolving a [`DeviceRoute`] means enumerating and opening, so every function
//! here needs a backend. The layer that implements them takes one explicitly —
//! that is what lets it be driven by a scripted device tree, or by another
//! host's HID stack. These are the same functions with *this* host's backend
//! supplied, for the overwhelmingly common caller who means "this machine".
//!
//! Channel-addressed operations (the `_on` family) need no backend and are not
//! wrapped: they act on a channel the caller already holds.

use openlogi_core::hid::{LightCommand, WriteError};

use crate::backlight::BacklightState;
use crate::channel::route::DeviceRoute;
use crate::channel::transport::native_backend;
use crate::smartshift::{SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus};
use crate::write::{
    self as device, Dpi, DpiInfo, FeatureEntry, HapticWaveform, LightingMethod, LitraModel,
    FirmwareEntity, ReprogControlEntry, ScrollResolution, ScrollWheelMode,
};

/// Read the sensor DPI of the device `route` reaches.
pub async fn get_dpi(route: &DeviceRoute) -> Result<Dpi, WriteError> {
    device::get_dpi(&*native_backend(), route).await
}

/// Read the DPI range and capabilities of the device `route` reaches.
pub async fn get_dpi_info(route: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    device::get_dpi_info(&*native_backend(), route).await
}

/// Write a new sensor DPI to the device `route` reaches.
pub async fn set_dpi(route: &DeviceRoute, dpi: Dpi) -> Result<(), WriteError> {
    device::set_dpi(&*native_backend(), route, dpi).await
}

/// Read the SmartShift mode, threshold and torque of the device `route` reaches.
pub async fn get_smartshift_status(route: &DeviceRoute) -> Result<SmartShiftStatus, WriteError> {
    device::get_smartshift_status(&*native_backend(), route).await
}

/// Write a full SmartShift status to the device `route` reaches.
pub async fn set_smartshift(
    route: &DeviceRoute,
    status: SmartShiftStatus,
) -> Result<(), WriteError> {
    device::set_smartshift(&*native_backend(), route, status).await
}

/// Flip the device `route` reaches between free-spin and ratchet.
pub async fn toggle_smartshift(route: &DeviceRoute) -> Result<SmartShiftMode, WriteError> {
    device::toggle_smartshift(&*native_backend(), route).await
}

/// Set the SmartShift auto-disengage sensitivity of the device `route` reaches.
pub async fn set_smartshift_sensitivity(
    route: &DeviceRoute,
    value: SmartShiftAutoDisengage,
) -> Result<SmartShiftStatus, WriteError> {
    device::set_smartshift_sensitivity(&*native_backend(), route, value).await
}

/// Read the scroll-wheel resolution and inversion of the device `route` reaches.
pub async fn get_scroll_wheel_mode(route: &DeviceRoute) -> Result<ScrollWheelMode, WriteError> {
    device::get_scroll_wheel_mode(&*native_backend(), route).await
}

/// Set the scroll-wheel resolution of the device `route` reaches.
pub async fn set_scroll_resolution(
    route: &DeviceRoute,
    resolution: ScrollResolution,
) -> Result<ScrollWheelMode, WriteError> {
    device::set_scroll_resolution(&*native_backend(), route, resolution).await
}

/// Set the scroll-wheel inversion of the device `route` reaches.
pub async fn set_scroll_inversion(route: &DeviceRoute, inverted: bool) -> Result<(), WriteError> {
    device::set_scroll_inversion(&*native_backend(), route, inverted).await
}

/// Set both scroll-wheel resolution and inversion in one pass.
pub async fn set_scroll_wheel_mode(
    route: &DeviceRoute,
    resolution: ScrollResolution,
    inverted: bool,
) -> Result<ScrollWheelMode, WriteError> {
    device::set_scroll_wheel_mode(&*native_backend(), route, resolution, inverted).await
}

/// Set the Fn-key inversion of the keyboard `route` reaches.
pub async fn set_fn_lock(route: &DeviceRoute, on: bool) -> Result<(), WriteError> {
    device::set_fn_lock(&*native_backend(), route, on).await
}

/// Read the backlight state of the keyboard `route` reaches.
pub async fn get_backlight(route: &DeviceRoute) -> Result<BacklightState, WriteError> {
    device::get_backlight(&*native_backend(), route).await
}

/// Turn the backlight of the keyboard `route` reaches on or off.
pub async fn set_backlight_enabled(
    route: &DeviceRoute,
    on: bool,
) -> Result<BacklightState, WriteError> {
    device::set_backlight_enabled(&*native_backend(), route, on).await
}

/// Set every key of the keyboard `route` reaches to one colour.
pub async fn set_keyboard_color(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    device::set_keyboard_color(&*native_backend(), route, r, g, b).await
}

/// Set every key to one colour over a chosen lighting feature.
pub async fn set_keyboard_color_with(
    route: &DeviceRoute,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    device::set_keyboard_color_with(&*native_backend(), route, method, r, g, b).await
}

/// Play a haptic waveform on the device `route` reaches.
pub async fn play_haptic(route: &DeviceRoute, waveform: HapticWaveform) -> Result<(), WriteError> {
    device::play_haptic(&*native_backend(), route, waveform).await
}

/// Apply a light command to the Litra `route` reaches.
pub async fn apply_litra(
    route: &DeviceRoute,
    model: LitraModel,
    command: LightCommand,
) -> Result<(), WriteError> {
    device::apply_litra(&*native_backend(), route, model, command).await
}

/// Walk the HID++ feature table of the device `route` reaches.
pub async fn dump_features(route: &DeviceRoute) -> Result<Vec<FeatureEntry>, WriteError> {
    device::dump_features(&*native_backend(), route).await
}

/// Walk the firmware entities of the device `route` reaches.
pub async fn dump_firmware_entities(
    route: &DeviceRoute,
) -> Result<Vec<FirmwareEntity>, WriteError> {
    device::dump_firmware_entities(&*native_backend(), route).await
}

/// Walk the reprogrammable controls of the device `route` reaches.
pub async fn dump_reprog_controls(
    route: &DeviceRoute,
) -> Result<Vec<ReprogControlEntry>, WriteError> {
    device::dump_reprog_controls(&*native_backend(), route).await
}

/// Read the raw battery report of the device `route` reaches.
pub async fn read_battery_raw(route: &DeviceRoute) -> Result<String, WriteError> {
    device::read_battery_raw(&*native_backend(), route).await
}

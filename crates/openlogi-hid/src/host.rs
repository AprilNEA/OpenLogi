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

use std::sync::Arc;

use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::{LightCommand, PairingError, WriteError};

use crate::probe_cache::FileProbeCacheStore;
use crate::transport::native_backend;
use openlogi_core::hid::smartshift::{SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus};
use openlogi_device::SharedChannel;
use openlogi_device::ChannelPool;
use openlogi_device::DeviceRoute;
use openlogi_device::backend::{HidBackend, HotplugStream};
use openlogi_device::backlight::BacklightState;
use openlogi_device::inventory::{Enumerator, InventoryError};
use openlogi_device::pairing::PairingReceiver;
use openlogi_device::write::{
    self as device, Dpi, DpiInfo, FeatureEntry, FirmwareEntity, HapticWaveform, LightingMethod,
    LitraModel, ReprogControlEntry, ScrollResolution, ScrollWheelMode,
};

/// This host's HID stack.
///
/// Public because the entry points with a wide parameter list — a capture
/// session, a pairing flow — are clearer passed a backend than wrapped again;
/// an agent that drives them names its backend once and hands it down.
#[must_use]
pub fn backend() -> Arc<dyn HidBackend> {
    native_backend()
}

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

/// Read the report rate and supported values of the device `route` reaches.
pub async fn get_report_rate_info(
    route: &DeviceRoute,
) -> Result<device::ReportRateInfo, WriteError> {
    device::get_report_rate_info(&*native_backend(), route).await
}

/// Write a new report rate to the device `route` reaches.
pub async fn set_report_rate(
    route: &DeviceRoute,
    rate: device::ReportRateHz,
) -> Result<(), WriteError> {
    device::set_report_rate(&*native_backend(), route, rate).await
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

/// Enumerate `0x8070` zones and whether per-key lighting is present.
pub async fn read_lighting_info(
    route: &DeviceRoute,
    mouse: bool,
    screen_sampler: bool,
    audio_visualizer: bool,
) -> Result<openlogi_core::hid::LightingInfo, WriteError> {
    device::read_lighting_info(
        &*native_backend(),
        route,
        mouse,
        screen_sampler,
        audio_visualizer,
    )
    .await
}

/// Apply a firmware lighting config, or report that a host renderer must run.
pub async fn apply_lighting(
    route: &DeviceRoute,
    lighting: &openlogi_core::config::Lighting,
) -> Result<openlogi_device::LightingApply, WriteError> {
    device::apply_lighting(&*native_backend(), route, lighting).await
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

/// Dump HID++ `0x8100` onboard profiles of the device `route` reaches.
pub async fn dump_onboard_profiles(
    route: &DeviceRoute,
) -> Result<device::OnboardProfilesDump, WriteError> {
    device::dump_onboard_profiles(&*native_backend(), route).await
}

/// Read the active onboard profile's buttons as OpenLogi actions.
pub async fn read_onboard_button_bindings(
    route: &DeviceRoute,
) -> Result<
    std::collections::BTreeMap<openlogi_core::binding::ButtonId, openlogi_core::binding::Action>,
    WriteError,
> {
    device::read_onboard_button_bindings(&*native_backend(), route).await
}

/// Read the active onboard profile of `route` (buttons, DPI slots, LEDs).
pub async fn read_onboard_profile(
    route: &DeviceRoute,
) -> Result<openlogi_core::hid::OnboardProfileSnapshot, WriteError> {
    device::read_onboard_profile(&*native_backend(), route).await
}

/// Write button bindings into the active onboard profile of `route`.
pub async fn apply_onboard_button_bindings(
    route: &DeviceRoute,
    bindings: &std::collections::BTreeMap<
        openlogi_core::binding::ButtonId,
        openlogi_core::binding::Action,
    >,
) -> Result<(), WriteError> {
    device::apply_onboard_button_bindings(&*native_backend(), route, bindings).await
}

/// Advance the active onboard profile stored in device flash for `route`.
pub async fn cycle_onboard_profile(route: &DeviceRoute) -> Result<u8, WriteError> {
    device::cycle_onboard_profile(&*native_backend(), route).await
}

/// Advance the active onboard profile on an already-open channel.
pub async fn cycle_onboard_profile_on(shared: &SharedChannel) -> Result<u8, WriteError> {
    device::cycle_onboard_profile_on(shared).await
}

/// Read the raw battery report of the device `route` reaches.
pub async fn read_battery_raw(route: &DeviceRoute) -> Result<String, WriteError> {
    device::read_battery_raw(&*native_backend(), route).await
}

/// An enumerator over this host's HID stack, with a memory-only probe cache.
///
/// One-shot callers (the CLI) want exactly this: nothing to warm-start from and
/// nothing left behind.
#[must_use]
pub fn enumerator() -> Enumerator {
    Enumerator::with_backend(native_backend())
}

/// An enumerator over this host's HID stack whose probe cache is the on-disk
/// one, so a device fully probed once keeps its identity across restarts.
///
/// Falls back to memory-only when no data dir resolves — a warm start is an
/// optimization, never a requirement.
#[must_use]
pub fn persisted_enumerator() -> Enumerator {
    let enumerator = enumerator();
    match FileProbeCacheStore::in_data_dir() {
        Some(store) => enumerator.with_probe_cache(Arc::new(store)),
        None => enumerator,
    }
}

/// A channel pool over this host's HID stack.
#[must_use]
pub fn channel_pool() -> ChannelPool {
    ChannelPool::with_backend(native_backend())
}

/// Enumerate this host's recognized standalone devices.
pub async fn enumerate_standalone() -> Result<Vec<StandaloneDevice>, InventoryError> {
    openlogi_device::inventory::standalone::enumerate_standalone(&*native_backend()).await
}

/// Subscribe to this host's HID hotplug events.
pub fn watch_hotplug() -> Result<HotplugStream, InventoryError> {
    openlogi_device::inventory::hotplug::watch_hotplug(&*native_backend())
}

/// Enumerate the HID++ receivers and paired devices on this host, once.
pub async fn enumerate() -> Result<Vec<DeviceInventory>, InventoryError> {
    openlogi_device::inventory::enumerate(native_backend()).await
}

/// List the pairing-capable receivers connected to this host.
pub async fn list_pairing_receivers() -> Result<Vec<PairingReceiver>, PairingError> {
    openlogi_device::pairing::list_pairing_receivers(&*native_backend()).await
}

//! HID++ device discovery and inspection for OpenLogi.
//!
//! Wraps the `hidpp` crate over `async-hid` as the transport. Public
//! entry points:
//!
//! - [`enumerate`] — one-shot inventory of receivers + paired devices.
//! - [`set_dpi`] — write a new sensor DPI to a connected device.

#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

mod channel_pool;
mod channel_registry;
pub mod host_switch;
mod mappings;
mod node_ledger;
mod route;
#[cfg(test)]
pub(crate) mod scripted_channel;
mod standalone;
mod transport;
// Native Win32 HID report-write fallback, used by the Windows composite channel
// in `transport` when async-hid's async write path fails.
#[cfg(target_os = "windows")]
mod windows_hid;

pub mod backlight;
pub mod gesture;
mod hires_wheel;
pub mod hotplug;
pub mod inventory;
pub mod keyboard;
pub mod pairing;
pub mod reprog_controls;
pub mod thumbwheel;
pub mod write;

/// SmartShift mode/status wire types. Pure data with no HID++ I/O, so they
/// live in `openlogi_core::hid::smartshift`; re-exported here as a module so
/// `crate::smartshift::X` keeps resolving for existing callers.
pub use openlogi_core::hid::smartshift;

pub use backlight::{BacklightMode, BacklightState, BacklightStatus};
pub use channel_pool::ChannelPool;
pub use channel_registry::ChannelRegistry;
pub use gesture::{
    CaptureChannel, CaptureStop, CapturedInput, GestureError, run_capture_session,
    run_capture_session_with_registry, run_capture_session_with_stop_reason,
};
pub use hires_wheel::{
    ScrollReportingTarget, ScrollResolution, ScrollWheelMode, get_scroll_wheel_mode,
    get_scroll_wheel_mode_on, set_scroll_inversion, set_scroll_inversion_on, set_scroll_resolution,
    set_scroll_resolution_on, set_scroll_wheel_mode, set_scroll_wheel_mode_on,
};
pub use host_switch::{
    HostSwitchError, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
};
pub use hotplug::{HotplugEvent, watch_hotplug};
pub use inventory::{Enumerator, InventoryError, enumerate};
pub use keyboard::{
    KEYBOARD_KEY_CIDS, run_keyboard_capture_session, run_keyboard_capture_session_with_registry,
};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use route::{
    BOLT_PIDS, DIRECT_DEVICE_INDEX, DeviceRoute, LIGHTSPEED_PIDS, UNIFYING_PIDS,
    receiver_display_name, speaks_unifying_protocol,
};
pub use smartshift::{AUTO_DISENGAGE_PERMANENT, SmartShiftMode, SmartShiftStatus};
pub use standalone::enumerate_standalone;
pub use write::{
    DpiCapabilities, DpiInfo, FeatureEntry, HapticWaveform, HidppFeatureErrorKind, HidppOperation,
    LightCommand, LightingMethod, LitraModel, ReprogControlEntry, SharedChannel, WriteError,
    apply_litra, clear_haptic_feature_cache, commands_for_light_settings, dump_features,
    dump_reprog_controls, encode_litra_command, ensure_haptics_armed_on, get_backlight, get_dpi,
    get_dpi_info, get_dpi_info_on, get_smartshift_status, get_smartshift_status_on, matches_litra,
    play_haptic, play_haptic_on, read_battery_raw, set_backlight_enabled, set_dpi, set_dpi_on,
    set_fn_lock, set_fn_lock_on, set_keyboard_color, set_keyboard_color_on,
    set_keyboard_color_with, set_keyboard_color_with_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, toggle_smartshift, toggle_smartshift_on,
};

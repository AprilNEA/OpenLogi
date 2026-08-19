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

mod channel;

pub mod backlight;
pub mod inventory;
pub mod pairing;
pub mod permissions;
pub mod reprog_controls;
pub mod session;
pub mod thumbwheel;
pub mod write;

/// SmartShift mode/status wire types. Pure data with no HID++ I/O, so they
/// live in `openlogi_core::hid::smartshift`; re-exported here as a module so
/// `crate::smartshift::X` keeps resolving for existing callers.
pub use openlogi_core::hid::smartshift;

pub use backlight::{BacklightMode, BacklightState, BacklightStatus};
pub use channel::route::{
    BOLT_PIDS, DIRECT_DEVICE_INDEX, DeviceRoute, LIGHTSPEED_PIDS, LOGITECH_VENDOR_ID,
    UNIFYING_PIDS, receiver_display_name, speaks_unifying_protocol,
};
pub use channel::{ChannelPool, ChannelRegistry, SharedChannel};
pub use hidpp::feature::FeatureType;
pub use hidpp::feature::device_information::DeviceEntityType;
pub use inventory::hotplug::{HotplugEvent, watch_hotplug};
pub use inventory::standalone::enumerate_standalone;
pub use inventory::{Enumerator, InventoryError, enumerate};
pub use openlogi_core::hid::{OnboardProfilesInfo, ProfileEntry, ProfilesMode, is_rom_sector};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use session::gesture::{
    CaptureChannel, CaptureStop, CapturedInput, GestureError, run_capture_session,
    run_capture_session_with_registry, run_capture_session_with_stop_reason,
};
pub use session::host_switch::{
    HostSwitchError, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
};
pub use session::keyboard::{
    KEYBOARD_KEY_CIDS, run_keyboard_capture_session, run_keyboard_capture_session_with_registry,
};
pub use smartshift::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, TunableTorque,
};
pub use write::{
    Dpi, DpiCapabilities, DpiInfo, FeatureEntry, FirmwareEntity, FirmwareEntityInfo,
    HapticWaveform, HidppFeatureErrorKind, HidppOperation, LITRA_BEAM_PRODUCT_ID,
    LITRA_GLOW_PRODUCT_ID, LightCommand, LightingMethod, LitraModel, ReprogControlEntry,
    ScrollReportingTarget, ScrollResolution, ScrollWheelMode, WriteError, apply_litra,
    apply_profiles_config, apply_profiles_config_on, clear_haptic_feature_cache,
    commands_for_light_settings, dump_features, dump_firmware_entities, dump_reprog_controls,
    encode_litra_command, ensure_haptics_armed_on, get_backlight, get_dpi, get_dpi_info,
    get_dpi_info_on, get_onboard_profiles, get_onboard_profiles_on, get_scroll_wheel_mode,
    get_scroll_wheel_mode_on, get_smartshift_status, get_smartshift_status_on, matches_litra,
    play_haptic, play_haptic_on, read_battery_raw, set_active_profile, set_backlight_enabled,
    set_dpi, set_dpi_on, set_fn_lock, set_fn_lock_on, set_keyboard_color, set_keyboard_color_on,
    set_keyboard_color_with, set_keyboard_color_with_on, set_profiles_mode, set_scroll_inversion,
    set_scroll_inversion_on, set_scroll_resolution, set_scroll_resolution_on,
    set_scroll_wheel_mode, set_scroll_wheel_mode_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, toggle_smartshift, toggle_smartshift_on,
};

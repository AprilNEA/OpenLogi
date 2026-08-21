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

/// The backend contract this crate implements over `async-hid`.
///
/// Defined in `openlogi-device`, which knows nothing of any host — re-exported
/// here so the path callers already use keeps resolving, and so a consumer
/// that only needs to *name* a backend need not decide which one.
pub use openlogi_device as backend;

pub mod backlight;
pub mod host;
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

pub use backend::{BackendError, HotplugEvent, NodeId, NodeInfo};
pub use backlight::{BacklightMode, BacklightState, BacklightStatus};
pub use channel::route::{
    BOLT_PIDS, DIRECT_DEVICE_INDEX, DeviceRoute, LIGHTSPEED_PIDS, LOGITECH_VENDOR_ID,
    UNIFYING_PIDS, receiver_display_name, speaks_unifying_protocol,
};
pub use channel::{ChannelPool, ChannelRegistry, SharedChannel};
pub use hidpp::feature::FeatureType;
pub use hidpp::feature::device_information::DeviceEntityType;
// `host` supplies this machine's backend to the entry points that need one;
// the types and the backend-taking originals stay reachable by module path.
pub use host::{enumerate, enumerate_standalone, list_pairing_receivers, watch_hotplug};
pub use inventory::{Enumerator, InventoryError};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, run_pairing, unpair,
};
pub use session::gesture::{
    CaptureChannel, CaptureStop, CapturedInput, GestureError, run_capture_session,
    run_capture_session_with_stop_reason,
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
// The route-addressed half comes from `host`, which supplies this machine's
// backend; the channel-addressed `_on` half needs none and comes straight from
// `write`.
pub use host::{
    apply_litra, dump_features, dump_firmware_entities, dump_reprog_controls, get_backlight,
    get_dpi, get_dpi_info, get_scroll_wheel_mode, get_smartshift_status, play_haptic,
    read_battery_raw, set_backlight_enabled, set_dpi, set_fn_lock, set_keyboard_color,
    set_keyboard_color_with, set_scroll_inversion, set_scroll_resolution, set_scroll_wheel_mode,
    set_smartshift, set_smartshift_sensitivity, toggle_smartshift,
};
pub use write::{
    Dpi, DpiCapabilities, DpiInfo, FeatureEntry, FirmwareEntity, FirmwareEntityInfo,
    HapticWaveform, HidppFeatureErrorKind, HidppOperation, LITRA_BEAM_PRODUCT_ID,
    LITRA_GLOW_PRODUCT_ID, LightCommand, LightingMethod, LitraModel, ReprogControlEntry,
    ScrollReportingTarget, ScrollResolution, ScrollWheelMode, WriteError,
    clear_haptic_feature_cache, commands_for_light_settings, encode_litra_command,
    ensure_haptics_armed_on, get_dpi_info_on, get_scroll_wheel_mode_on, get_smartshift_status_on,
    matches_litra, play_haptic_on, set_dpi_on, set_fn_lock_on, set_keyboard_color_on,
    set_keyboard_color_with_on, set_scroll_inversion_on, set_scroll_resolution_on,
    set_scroll_wheel_mode_on, set_smartshift_on, toggle_smartshift_on,
};

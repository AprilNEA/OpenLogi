//! OpenLogi's HID++ device layer: everything that knows Logitech's protocol
//! and nothing about a host.
//!
//! [`backend::HidBackend`] is the seam. Above it sits this crate — enumeration
//! and probing, the write layer, capture sessions, pairing. Below it sits one
//! implementation per host HID API: `openlogi-hid` over `async-hid`, a
//! scripted device tree in tests, WebHID under wasm if that is ever built.
//!
//! Nothing here opens a device by itself; everything that needs one is handed
//! a backend. That is what makes the layer reusable, and it is checked rather
//! than trusted — CI's `wasm (portable crates)` job builds this crate for a
//! target with no OS underneath it, so a dependency that assumes one fails
//! there and nowhere else.

#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

mod channel;
mod device_io;
mod keypad_vendor;

pub mod backend;
pub mod backlight;
pub mod host_lock;
pub mod inventory;
pub mod pairing;
pub mod replay;
pub mod reprog_controls;
pub mod session;
pub mod thumbwheel;
pub mod write;

pub use backend::{
    BackendError, HidBackend, HotplugEvent, HotplugStream, NodeId, NodeInfo, RawWriter,
};
pub use backlight::{
    BacklightChange, BacklightControls, BacklightDuration, BacklightMode, BacklightState,
    BacklightStatus,
};
pub use channel::route::{
    DIRECT_DEVICE_INDEX, DeviceRoute, LOGITECH_VENDOR_ID, RECEIVERS, ReceiverBrand,
    ReceiverDescriptor, ReceiverProtocol, find_receiver, receiver_display_name,
    speaks_unifying_protocol,
};
pub use channel::{ChannelPool, ChannelRegistry, SharedChannel, WeakSharedChannel};
pub use device_io::{DeviceIoGate, DeviceIoSignal, IoSuspended, device_io_channel};
pub use inventory::hotplug::watch_hotplug;
pub use inventory::standalone::enumerate_standalone;
pub use inventory::{Enumerator, InventoryError, enumerate};
pub use openlogi_core::hid::FnLockState;
pub use openlogi_core::hid::smartshift;
pub use openlogi_core::hid::smartshift::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, TunableTorque,
};
pub use openlogi_core::hid::{OnboardMode, OnboardProfile, OnboardState};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use session::gesture::{
    CaptureChannelSlot, CaptureError, CaptureHost, CaptureSessionFailure, CaptureSessionOutcome,
    CaptureSessionStop, CapturedInput, PendingCaptureRestore, run_capture_session,
};
pub use session::host_switch::{
    FollowerLink, FollowerRelease, HostSwitchError, HostSwitchFollowUp, HostSwitchRestoreOutcome,
    HostSwitchSessionFailure, HostSwitchSessionOutcome, HostSwitchStopReason, LeadCookie,
    PendingHostSwitchRestore, link_follower_cookies_on, release_follower_cookies_on,
    run_host_switch_session, switch_linked_hosts,
};
pub use session::keyboard::{
    KeyboardWanted, RESERVED_KEYBOARD_CONTROLS, is_reserved_keyboard_control,
    run_keyboard_capture_session,
};
pub use write::{
    ButtonSpyError, ButtonSpyWatch, DisableKeysMask, DisableKeysState, Dpi, DpiCapabilities,
    DpiInfo, DpiLed, DpiTarget, EasySwitchHosts, EasySwitchSlot, FeatureEntry, FirmwareEntity,
    FirmwareEntityInfo, ForceSensingInfo, HapticRestore, HapticRestoreOutcome, HapticWaveform,
    HidppFeatureErrorKind, HidppOperation, HostConnection, HostOs, HostOsInfo, HostOsKind,
    KeyDisplay, KeyImageJpeg, LITRA_BEAM_PRODUCT_ID, LITRA_GLOW_PRODUCT_ID, LightChange,
    LightCommand, LightControls, LightWatch, LightingMethod, LitraDescriptor, LitraModel,
    OsLayoutOutcome, OsMask, OsVersion, PointerSpeed, RAW_BATTERY_FEATURES, ReportRate,
    ReportRateInfo, ReprogControlEntry, ScrollReportingTarget, ScrollResolution, ScrollWheelMode,
    WriteError, apply_litra, commands_for_light_settings, dump_features, dump_firmware_entities,
    dump_reprog_controls, encode_litra_command, ensure_haptics_armed_on, find_litra, get_backlight,
    get_backlight_on, get_disable_keys, get_disable_keys_on, get_dpi, get_dpi_info,
    get_dpi_info_on, get_easy_switch_hosts, get_easy_switch_hosts_on, get_fn_lock, get_fn_lock_on,
    get_force_sensing_on, get_onboard, get_onboard_on, get_pointer_speed_on, get_report_rate_info,
    get_report_rate_info_on, get_scroll_wheel_mode, get_scroll_wheel_mode_on,
    get_smartshift_status, get_smartshift_status_on, keep_os_layout_on, key_display_for,
    litra_model_for_route, matches_litra, play_haptic, play_haptic_on, read_battery_raw,
    read_litra_state, rename_easy_switch_host, rename_easy_switch_host_on, restore_haptics_on,
    set_disable_keys, set_disable_keys_on, set_disable_keys_write_through_on, set_dpi,
    set_dpi_led_on, set_dpi_on, set_dpi_target_on, set_fn_lock, set_fn_lock_on,
    set_force_sensing_on, set_keyboard_color, set_keyboard_color_on, set_keyboard_color_with,
    set_keyboard_color_with_on, set_pointer_speed_on, set_report_rate, set_report_rate_on,
    set_scroll_inversion, set_scroll_inversion_on, set_scroll_resolution, set_scroll_resolution_on,
    set_scroll_wheel_mode, set_scroll_wheel_mode_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, set_smartshift_torque, toggle_smartshift, toggle_smartshift_on,
    update_backlight, update_backlight_on, watch_button_spy, watch_litra, write_key_image_on,
};

/// Scripted transport helpers for tests in higher layers.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub use crate::channel::scripted::{publish_scripted_channel, scripted_shared_channel};
    pub use crate::replay::ReplayChannelHandle as ScriptedRawHidHandle;
}

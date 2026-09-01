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

pub mod backend;
pub mod backlight;
pub mod inventory;
pub mod pairing;
pub mod reprog_controls;
pub mod session;
pub mod thumbwheel;
pub mod write;

pub use backend::{
    BackendError, HidBackend, HotplugEvent, HotplugStream, NodeId, NodeInfo, RawWriter,
};
pub use backlight::{BacklightMode, BacklightState, BacklightStatus};
pub use channel::route::{
    DIRECT_DEVICE_INDEX, DeviceRoute, LOGITECH_VENDOR_ID, RECEIVERS, ReceiverBrand,
    ReceiverDescriptor, ReceiverProtocol, find_receiver, receiver_display_name,
    speaks_unifying_protocol,
};
pub use channel::{ChannelPool, ChannelRegistry, SharedChannel};
pub use device_io::{DeviceIoGate, DeviceIoSignal, device_io_channel};
pub use inventory::hotplug::watch_hotplug;
pub use inventory::standalone::enumerate_standalone;
pub use inventory::{Enumerator, InventoryError, enumerate};
pub use openlogi_core::hid::smartshift;
pub use openlogi_core::hid::smartshift::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, TunableTorque,
};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use session::gesture::{
    CaptureChannel, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput, GestureError,
    PendingCaptureRestore, run_capture_session, run_capture_session_with_registry_spec,
};
pub use session::host_switch::{
    HostSwitchError, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
};
pub use session::keyboard::{
    KEYBOARD_KEY_CIDS, run_keyboard_capture_session, run_keyboard_capture_session_with_registry,
};
pub use write::{
    Dpi, DpiCapabilities, DpiInfo, FeatureEntry, FirmwareEntity, FirmwareEntityInfo,
    HapticWaveform, HidppFeatureErrorKind, HidppOperation, LITRA_BEAM_PRODUCT_ID,
    LITRA_GLOW_PRODUCT_ID, LightCommand, LightingMethod, LitraDescriptor, LitraModel,
    ReprogControlEntry, ScrollReportingTarget, ScrollResolution, ScrollWheelMode, WriteError,
    apply_litra, commands_for_light_settings, dump_features, dump_firmware_entities,
    dump_reprog_controls, encode_litra_command, ensure_haptics_armed_on, find_litra, get_backlight,
    get_dpi, get_dpi_info, get_dpi_info_on, get_scroll_wheel_mode, get_scroll_wheel_mode_on,
    get_smartshift_status, get_smartshift_status_on, litra_model_for_route, matches_litra,
    play_haptic, play_haptic_on, read_battery_raw, set_backlight_enabled, set_dpi, set_dpi_on,
    set_fn_lock, set_fn_lock_on, set_keyboard_color, set_keyboard_color_on,
    set_keyboard_color_with, set_keyboard_color_with_on, set_scroll_inversion,
    set_scroll_inversion_on, set_scroll_resolution, set_scroll_resolution_on,
    set_scroll_wheel_mode, set_scroll_wheel_mode_on, set_smartshift, set_smartshift_on,
    set_smartshift_sensitivity, toggle_smartshift, toggle_smartshift_on,
};

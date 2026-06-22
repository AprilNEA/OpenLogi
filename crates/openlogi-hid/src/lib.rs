//! HID++ device discovery and inspection for OpenLogi.
//!
//! Wraps the `hidpp` crate over `async-hid` as the transport. Public
//! entry points:
//!
//! - [`enumerate`] — one-shot inventory of receivers + paired devices.
//! - [`set_dpi`] — write a new sensor DPI to a connected device.

mod mappings;
mod node_ledger;
mod route;
mod transport;
// Native Win32 HID report-write fallback, used by the Windows composite channel
// in `transport` when async-hid's async write path fails.
#[cfg(target_os = "windows")]
mod windows_hid;

pub mod gesture;
pub mod inventory;
pub mod pairing;
pub mod reprog_controls;
pub mod smartshift;
pub mod thumbwheel;
pub mod write;

pub use gesture::{CaptureChannel, CapturedInput, GestureError, run_capture_session};
pub use inventory::{Enumerator, InventoryError, enumerate};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use route::{BOLT_PIDS, DIRECT_DEVICE_INDEX, DeviceRoute, UNIFYING_PIDS};
pub use smartshift::{AUTO_DISENGAGE_PERMANENT, SmartShiftMode, SmartShiftStatus};
pub use write::{
    DpiCapabilities, DpiInfo, FeatureEntry, LightingMethod, SharedChannel, WriteError,
    dump_features, get_dpi, get_dpi_info, get_smartshift_status, set_dpi, set_dpi_on,
    set_keyboard_color, set_keyboard_color_with, set_scroll_inversion, set_scroll_inversion_on,
    set_smartshift, set_smartshift_on, set_smartshift_sensitivity, toggle_smartshift,
    toggle_smartshift_on,
};

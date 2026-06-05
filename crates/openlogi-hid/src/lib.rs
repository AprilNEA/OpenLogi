//! HID++ device discovery and inspection for OpenLogi.
//!
//! Wraps the `hidpp` crate over `async-hid` as the transport. Public
//! entry points:
//!
//! - [`enumerate`] — one-shot inventory of receivers + paired devices.
//! - [`set_dpi`] — write a new sensor DPI to a connected device.

mod route;
mod transport;

pub mod gesture;
pub mod inventory;
pub mod pairing;
pub mod reprog_controls;
pub mod smartshift;
pub mod thumbwheel;
pub mod write;

pub use gesture::{CaptureChannel, CapturedInput, GestureError, run_capture_session};
pub use inventory::{InventoryError, enumerate};
pub use pairing::{
    Click, DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver,
    PasskeyMethod, ReceiverFamily, ReceiverSelector, list_pairing_receivers, run_pairing, unpair,
};
pub use route::{DIRECT_DEVICE_INDEX, DeviceRoute};
pub use smartshift::{AUTO_DISENGAGE_PERMANENT, SmartShiftMode, SmartShiftStatus};
pub use write::{
    DpiCapabilities, DpiInfo, FeatureEntry, SharedChannel, WriteError, dump_features, get_dpi,
    get_dpi_info, get_smartshift_status, set_dpi, set_dpi_on, set_keyboard_color, set_smartshift,
    set_smartshift_on, toggle_smartshift, toggle_smartshift_on,
};

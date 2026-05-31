//! HID++ device discovery and inspection for OpenLogi.
//!
//! Wraps the `hidpp` crate over `async-hid` as the transport. Public
//! entry points:
//!
//! - [`enumerate`] — one-shot inventory of receivers + paired devices.
//! - [`set_dpi`] — write a new sensor DPI to a connected device.

mod route;
mod transport;

pub mod adjustable_dpi;
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
pub use smartshift::{SmartShiftMode, SmartShiftStatus};
pub use write::{
    FeatureEntry, SharedChannel, WriteError, dump_features, get_dpi, get_smartshift_status,
    set_dpi, set_dpi_on, toggle_smartshift, toggle_smartshift_on,
};

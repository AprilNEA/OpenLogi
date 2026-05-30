//! HID++ device discovery and inspection for OpenLogi.
//!
//! Wraps the `hidpp` crate over `async-hid` as the transport. Public
//! entry points:
//!
//! - [`enumerate`] — one-shot inventory of receivers + paired devices.
//! - [`set_dpi`] — write a new sensor DPI to a connected device.

mod transport;

pub mod adjustable_dpi;
pub mod gesture;
pub mod inventory;
pub mod reprog_controls;
pub mod smartshift;
pub mod write;

pub use gesture::{GestureError, GestureTarget, run_gesture_session};
pub use inventory::{InventoryError, enumerate};
pub use smartshift::{SmartShiftMode, SmartShiftStatus};
pub use write::{
    FeatureEntry, WriteError, dump_features, get_dpi, get_smartshift_status, set_dpi,
    toggle_smartshift,
};

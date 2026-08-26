//! Wire-format types for HID++ device control that need no device I/O.
//!
//! These are the addressing scheme ([`route`]), read-back snapshots (DPI,
//! SmartShift), and command/error vocabularies that cross the agent↔GUI IPC
//! boundary. The actual HID++ transport lives in the sibling `openlogi-hid`
//! crate, which re-exports every type here unchanged so its existing callers
//! keep using `openlogi_hid::X`; the GUI (a pure IPC client with no device
//! I/O of its own) depends on this module directly instead of linking the
//! HID stack.

pub mod dpi;
pub mod error;
pub mod light;
pub mod pairing;
pub mod route;
pub mod smartshift;

pub use dpi::{Dpi, DpiCapabilities, DpiInfo};
pub use error::{HidppFeatureErrorKind, HidppOperation, WriteError};
pub use light::{LightCommand, commands_for_light_settings};
pub use pairing::{Click, PairingError, PasskeyMethod, ReceiverSelector};
pub use route::{
    DIRECT_DEVICE_INDEX, DeviceRoute, LOGITECH_VENDOR_ID, RECEIVERS, ReceiverBrand,
    ReceiverDescriptor, ReceiverProtocol, find_receiver, is_receiver_pid, receiver_display_name,
    speaks_unifying_protocol,
};
pub use smartshift::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, TunableTorque,
};

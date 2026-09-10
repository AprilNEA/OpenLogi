//! Compatibility re-exports for semantic keyboard-backlight types.
//!
//! The pure domain types live in `openlogi-core`; HID++ conversion and
//! transport remain in this crate's write layer.

pub use openlogi_core::hid::backlight::{BacklightMode, BacklightState, BacklightStatus};

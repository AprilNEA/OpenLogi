//! Headless orchestration shared by the OpenLogi background agent and
//! (transitionally) the GUI.
//!
//! Everything here is GUI-free: the CGEventTap hook runtime, background HID++
//! writes, DPI-cycle state, and the Actions Ring's runtime session state. It
//! was extracted from `openlogi-gui` so the always-on agent process can own
//! the input/device path without linking gpui.
//!
//! The pure binding-map, device-ordering, and Actions-Ring-timing helpers
//! shared with the GUI have moved to `openlogi-core`; the tarpc contract
//! (`ipc`) and its transport are next to follow into a dedicated leaf crate.

pub mod action_ring;
pub mod capture_plan;
mod dpi;
pub mod event_monitor;
pub mod hardware;
pub mod hook_runtime;
pub mod ipc;
pub mod orchestrator;
pub mod receiver_access;
pub mod transport;
pub mod watchers;

pub use dpi::{DpiCycleState, DpiCycles};

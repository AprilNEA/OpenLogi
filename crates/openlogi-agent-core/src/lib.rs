//! Headless orchestration for the OpenLogi background agent.
//!
//! Everything here is GUI-free: the CGEventTap hook runtime, background HID++
//! writes, DPI-cycle state, and the Actions Ring's runtime session state. It
//! was extracted from `openlogi-desktop` so the always-on agent process can own
//! the input/device path without linking gpui.
//!
//! The wire contract this agent answers over IPC lives in `openlogi-ipc`, and
//! the pure binding-map / device-ordering / Actions-Ring-timing helpers
//! shared with the GUI live in `openlogi-core` — splitting those out of this
//! crate is what keeps the GUI from linking `openlogi-hid`/`hidpp`/`async-hid`.

pub mod action_ring;
pub mod capture_plan;
mod dpi;
pub mod event_monitor;
pub mod hardware;
pub mod hook_runtime;
pub mod observable;
pub mod orchestrator;
pub mod receiver_access;
pub mod watchers;

pub use dpi::{DpiCycleState, DpiCycles};

//! Headless orchestration shared by the OpenLogi background agent and
//! (transitionally) the GUI.
//!
//! Everything here is GUI-free: the CGEventTap hook runtime, background HID++
//! writes, DPI-cycle state, and binding-map construction. It was extracted from
//! `openlogi-gui` so the always-on agent process can own the input/device path
//! without linking gpui.

pub mod bindings;
pub mod device_order;
mod dpi;
pub mod event_monitor;
pub mod hardware;
pub mod hook_runtime;
pub mod ipc;
pub mod orchestrator;
pub mod receiver_access;
pub mod ring;
pub mod transport;
pub mod watchers;

pub use dpi::DpiCycleState;

//! Background watchers that poll external state — HID inventory, foreground
//! app, Accessibility, device pairing — and forward changes over channels to a
//! consumer (the agent's orchestrator, or the GUI).

pub mod accessibility;
pub mod camera;
mod capture_session;
pub mod foreground_app;
pub mod gesture;
pub mod host_switch;
pub mod input_monitoring;
pub mod inventory;
pub mod keyboard;
pub mod pairing;
mod poll;

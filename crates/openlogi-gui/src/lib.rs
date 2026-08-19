//! Code shared by the two binaries this crate ships: the settings app
//! (`openlogi-gui`) and the Actions Ring overlay helper (`openlogi-overlay`).
//!
//! Only what both processes genuinely need lives here. The overlay is a pure
//! IPC client with no settings UI, so the app's views, state, and platform
//! integration stay private to `main.rs`.

pub mod action_ring;
pub mod app_assets;
pub mod locale;

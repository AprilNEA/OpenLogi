//! Shared types and configuration for OpenLogi.
//!
//! Everything here is data — the device model, the action catalogue, the
//! binding types, the shape of the config file. It must never depend on
//! `hidpp`, `async-hid`, or any platform-specific event/window API; those live
//! in sibling crates.
//!
//! The exceptions are feature-gated (both on by default): reading and writing
//! that config file (`fs`), and locale negotiation (`locale`), which reads the
//! host's language preference. Without them this crate touches no host at all,
//! which is what the `wasm (portable crates)` CI job checks.

#![deny(missing_docs)]

pub mod action_ring;
pub mod app;
pub mod binding;
pub mod bindings;
pub mod brand;
pub mod color;
pub mod config;
pub mod device;
pub mod device_order;
pub mod diagnostics;
pub mod hid;
#[cfg(feature = "locale")]
pub mod locale;
#[cfg(feature = "fs")]
pub mod paths;
pub mod scroll;
#[cfg(feature = "fs")]
pub mod single_instance;

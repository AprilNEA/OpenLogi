//! Shared types and configuration for OpenLogi.
//!
//! This crate is deliberately I/O-free apart from filesystem reads/writes of
//! the user config file. It must never depend on `hidpp`, `async-hid`, or any
//! platform-specific event/window API — those live in sibling crates.

#![deny(missing_docs)]

pub mod action_ring;
mod app_selector;
pub mod binding;
pub mod bindings;
pub mod brand;
pub mod color;
pub mod config;
pub mod device;
pub mod device_order;
pub mod diagnostics;
pub mod hid;
pub mod paths;
pub mod single_instance;

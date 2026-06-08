//! Shared types and configuration for OpenLogi.
//!
//! This crate is deliberately I/O-free apart from filesystem reads/writes of
//! the user config file. It must never depend on `hidpp`, `async-hid`, or any
//! platform-specific event/window API — those live in sibling crates.

pub mod binding;
pub mod brand;
pub mod config;
pub mod device;
pub mod paths;
pub mod single_instance;

//! Hardware identities shared across OpenLogi's protocol, device, and host
//! layers.
//!
//! This crate records facts needed on both sides of dependency boundaries:
//! exact USB/HID identities, marketed product families, protocol families,
//! driver families, and asset-registry model IDs. Protocol encoding, host I/O,
//! and runtime capabilities stay with their owning crates.

#![no_std]
#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod litra;
pub mod receiver;

/// Logitech's USB/Bluetooth vendor ID.
pub const LOGITECH_VENDOR_ID: u16 = 0x046d;

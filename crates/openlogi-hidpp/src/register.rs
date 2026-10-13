//! Registers of HID++ 1.0 peripherals.
//!
//! A HID++ 1.0 device has no feature table: what it can be asked lives at
//! fixed register addresses, read with [`HidppChannel::read_register`]. Which
//! registers a device implements is not announced either; one it lacks is
//! refused with [`ErrorType::InvalidAddress`].
//!
//! [`HidppChannel::read_register`]: crate::channel::HidppChannel::read_register
//! [`ErrorType::InvalidAddress`]: crate::protocol::v10::ErrorType::InvalidAddress

pub mod battery;

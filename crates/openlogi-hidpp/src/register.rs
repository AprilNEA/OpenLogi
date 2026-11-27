//! Registers of HID++ 1.0 peripherals.
//!
//! A HID++ 1.0 device has no feature table: what it can be asked lives at
//! fixed register addresses, read with [`HidppChannel::read_register`] (three
//! bytes) or [`HidppChannel::read_long_register`] (sixteen). Which registers
//! a device implements is not announced either; one it lacks is refused with
//! [`ErrorType::InvalidAddress`].
//!
//! None of these layouts is in a published Logitech spec. Each module names
//! the reverse-engineered source it follows.
//!
//! [`HidppChannel::read_register`]: crate::channel::HidppChannel::read_register
//! [`HidppChannel::read_long_register`]: crate::channel::HidppChannel::read_long_register
//! [`ErrorType::InvalidAddress`]: crate::protocol::v10::ErrorType::InvalidAddress

pub mod battery;
pub mod current_profile;
pub mod onboard_memory;
pub mod report_rate;
pub mod resolution;

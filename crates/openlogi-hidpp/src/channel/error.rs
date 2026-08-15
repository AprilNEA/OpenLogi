//! The errors a HID or HID++ channel operation can fail with.

use std::error::Error;

use thiserror::Error;

/// Represents an error that occurred when creating or interacting with a HID or
/// HID++ communication channel.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// Indicates that the concrete implementation of
    /// [`RawHidChannel`](super::RawHidChannel) returned an error.
    #[error("the HID channel implementation returned an error")]
    Implementation(#[from] Box<dyn Error + Sync + Send>),

    /// Indicates that the HID report descriptor could not be parsed.
    #[error("the report descriptor could not be parsed")]
    ReportDescriptor(hidreport::ParserError),

    /// Indicates that the channel in question does not support HID++.
    #[error("the HID channel does not support HID++")]
    HidppNotSupported,

    /// Indicates that the HID++ channel does not support messages of the given
    /// type (short/long).
    #[error("the channel does not support the given HID++ message type")]
    MessageTypeNotSupported,

    /// Indicates that a raw output report was empty or exceeded 64 bytes.
    #[error("raw HID reports must contain 1..=64 bytes, got {0}")]
    InvalidRawReportLength(usize),

    /// Indicates that no response was received following a request.
    #[error("the device did not respond to the request")]
    NoResponse,

    /// Indicates that a bounded channel operation did not complete — typically
    /// because the device is asleep, out of range, connected to another host,
    /// or its transport write is wedged. See
    /// [`HidppChannel::send_with_timeout`](super::HidppChannel::send_with_timeout)
    /// and
    /// [`HidppChannel::write_raw_report`](super::HidppChannel::write_raw_report).
    #[error("the HID channel operation timed out")]
    Timeout,
}

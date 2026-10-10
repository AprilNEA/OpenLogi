//! Wire-format types for the Bolt/Unifying pairing flow — pure data, no I/O.
//!
//! The pairing session itself (discovery, notification decoding, the
//! register writes) lives in `openlogi_hid::pairing`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Selects which receiver a pairing operation targets.
///
/// Crosses the agent↔GUI IPC (`start_pairing`), so variant order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReceiverSelector {
    /// The first supported receiver found — fine for the common single-receiver case.
    First,
    /// A specific Bolt receiver by its unique ID.
    BoltUid(String),
}

/// A single click in a pointer passkey sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Click {
    /// Left mouse button click.
    Left,
    /// Right mouse button click.
    Right,
}

/// How the user authenticates the device during Bolt pairing.
///
/// Crosses the agent↔GUI IPC (inside `PairingUpdate::Passkey`, [`Click`]
/// included), so variant and field order are wire format — changes require a
/// `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasskeyMethod {
    /// Type these digits on the new keyboard, then press Enter.
    Keyboard(String),
    /// On the new pointer, perform this left/right click sequence, then click
    /// both buttons together.
    Pointer {
        /// Numeric passkey shown by the device.
        passkey: String,
        /// MSB-first click sequence derived from the passkey.
        clicks: Vec<Click>,
    },
}

/// Errors raised by pairing operations.
///
/// Pure data — no `hidpp` or HID-backend types — but not itself a wire type:
/// the agent maps it to `openlogi_ipc::PairingFailure`, which crosses
/// the IPC boundary. The conversion from `openlogi_hid::BackendError` lives in
/// `openlogi_hid::pairing`, which this crate must never depend on.
#[derive(Clone, Debug, Error)]
pub enum PairingError {
    /// HID transport failure.
    #[error("HID transport error: {0}")]
    Hid(String),
    /// No supported receiver matched the requested selector.
    #[error("no supported pairing-capable receiver found")]
    ReceiverNotFound,
    /// HID++ receiver register read/write failed.
    #[error("receiver register access failed: {0}")]
    Register(String),
    /// Pairing flow exceeded its timeout.
    #[error("pairing timed out")]
    Timeout,
    /// Receiver reported a device-specific pairing error code.
    #[error("receiver reported pairing error {0:#04x}")]
    Device(u8),
    /// Pairing flow was cancelled by the caller.
    #[error("pairing was cancelled")]
    Cancelled,
    /// The command is not valid for the active receiver family.
    #[error("pairing command is not supported by the active receiver")]
    UnsupportedCommand,
    /// A receiver notification failed to decode; authentication cannot
    /// proceed safely, so the flow fails instead of presenting bogus data.
    #[error("malformed pairing notification ({0})")]
    MalformedNotification(&'static str),
}

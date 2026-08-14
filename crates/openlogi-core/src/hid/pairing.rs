//! Wire-format types for the Bolt/Unifying pairing flow — pure data, no I/O.
//!
//! The pairing session itself (discovery, notification decoding, the
//! register writes) lives in `openlogi_hid::pairing`.

use serde::{Deserialize, Serialize};

/// Selects which receiver a pairing operation targets.
///
/// Crosses the agent↔GUI IPC (`start_pairing`), so variant order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
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
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Clone, Debug, Serialize, Deserialize)]
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

//! Keyboard Fn-lock read-back state — pure data, no I/O.
//!
//! The HID++ `0x40a2` / `0x40a3` reads and writes that produce this state
//! remain in `openlogi-device`.

use serde::{Deserialize, Serialize};

/// What a keyboard reports about its Fn-lock: whether a bare F-key currently
/// sends F1–F12 (`fn_lock`), and what it does out of the box.
///
/// Crosses the agent↔GUI IPC, so field order is wire format — changes require
/// a `PROTOCOL_VERSION` bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FnLockState {
    /// `true` when a bare F-key sends F1–F12 and Fn+F-key the printed
    /// function; `false` when the printed function comes first.
    pub fn_lock: bool,
    /// The firmware default for this host, before any software or Fn+Esc
    /// change.
    pub default_fn_lock: bool,
}

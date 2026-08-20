//! The haptic waveform vocabulary — pure data, no I/O.
//!
//! The firmware side is HID++ `0x19b0` (`playWaveform`), whose waveform IDs
//! live in the vendored `hidpp` fork. This enum is deliberately *not* that
//! one: it crosses the agent↔GUI IPC boundary, and the wire contract is
//! append-only (`.claude/rules/ipc-protocol.md`), so the protocol crate must
//! not be free to reorder its variants. `openlogi_hid` maps this to the
//! firmware ID at the point of the write.

use serde::{Deserialize, Serialize};

/// A haptic waveform the device firmware can play.
///
/// Only the two waveforms OpenLogi has confirmed on real hardware are modeled;
/// `0x19b0` is reverse-engineered, so an unverified ID is a silent no-op at
/// best. **Append new variants only** — serde encodes the declaration index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HapticWaveform {
    /// A light boundary pulse. Used for Actions Ring hover transitions —
    /// the "you crossed into the next slot" tick.
    SubtleCollision,
    /// A firmer confirmation pulse. Used when an Actions Ring action runs.
    DampStateChange,
}

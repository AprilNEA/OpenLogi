//! A haptic pulse a client asks the agent to play — pure data, no I/O.
//!
//! The HID++ `0x19b0` play that carries it lives in
//! `openlogi_device::write::haptic`, which knows the firmware waveform IDs;
//! the agent converts at that boundary.

use serde::{Deserialize, Serialize};

/// One of the waveforms HID++ `0x19b0 HapticFeedback` plays, named as the
/// firmware names them.
///
/// Variant order is wire format: append only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HapticPulse {
    /// A light tick: the Actions Ring plays it when the highlighted slot
    /// changes.
    SubtleCollision,
    /// A confirmation pulse: the Actions Ring plays it when an action runs.
    DampStateChange,
}

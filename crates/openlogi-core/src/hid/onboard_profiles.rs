//! Read-only snapshot of a HID++ `0x8100 OnboardProfiles` device's active
//! button bindings — pure data, no I/O.
//!
//! The reads that produce an [`OnboardProfileBindings`] live in
//! `openlogi_device::write::diagnostics` (protocol details, byte layouts,
//! and citations in `hidpp::feature::onboard_profiles`); this module only
//! carries the already-decoded, human-readable result across the agent/GUI
//! IPC boundary. G-series gaming mice (G502 X, G502 X LIGHTSPEED, ...)
//! expose this feature instead of `ReprogControls` (`0x1b00`–`0x1b04`), so
//! `Capabilities::buttons` never becomes true for them — see
//! [`super::super::device::Capabilities::onboard_profiles`].

use serde::{Deserialize, Serialize};

/// One decoded entry from the active profile's button-binding table.
///
/// `slot` is the entry's raw index into that table — not a confirmed
/// physical-button identity (no cited mapping from slot index to a named
/// button exists yet; see `hidpp::feature::onboard_profiles` module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileBinding {
    /// Index into the button-binding table this entry came from.
    pub slot: u8,
    /// Human-readable rendering of the decoded binding (e.g. "Mouse button
    /// 1", "Special: G-Shift", "Keyboard: T"). Not localized — this is
    /// diagnostic output describing raw device data, not UI copy.
    pub description: String,
}

/// The active onboard profile's decoded button bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileBindings {
    /// Which profile (1-based, matching the device's own numbering) these
    /// bindings came from. `None` when the device reported no active
    /// profile (onboard mode off, e.g. driven by G Hub instead).
    pub active_profile: Option<u8>,
    /// The decoded bindings, in table order, up to the first disabled slot.
    pub bindings: Vec<OnboardProfileBinding>,
}

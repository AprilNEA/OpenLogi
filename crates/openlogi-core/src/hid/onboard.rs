//! Gaming-mouse onboard memory. Wire types: field order matters.

use serde::{Deserialize, Serialize};

/// Who owns a gaming mouse's buttons, DPI levels and report rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnboardMode {
    /// A stored profile.
    Onboard,
    /// OpenLogi.
    Host,
}

/// One onboard profile slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfile {
    /// 1-based slot number.
    pub index: u8,
    /// Stored name.
    pub name: Option<String>,
    /// Whether it can be made active.
    pub enabled: bool,
}

/// Onboard memory as read from the mouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardState {
    /// Current owner.
    pub mode: OnboardMode,
    /// Active profile; `None` in host mode.
    pub active_profile: Option<u8>,
    /// Profile slots.
    pub profiles: Vec<OnboardProfile>,
}

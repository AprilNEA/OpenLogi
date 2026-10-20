//! Onboard memory read-back and edits — pure data, no I/O.
//!
//! A gaming mouse with HID++ `0x8100 OnboardProfiles` keeps profiles in its
//! own memory and runs one of them unless host software takes over. The
//! HID++ reads and writes that produce an [`OnboardMemory`] live in
//! `openlogi_device::write::onboard`.

use serde::{Deserialize, Serialize};

use super::{Dpi, ReportRate};
use crate::color::Rgb;

/// Where a mouse with onboard memory takes its settings from.
///
/// Serde enums ride the IPC wire: variants are append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// The mouse runs the profile stored in its memory, on any computer and
    /// whether OpenLogi runs or not. Edits are written to that profile.
    /// What the mouse does by itself after every power-up, so the default.
    #[default]
    Onboard,
    /// OpenLogi drives the mouse live from its saved settings while it runs;
    /// without OpenLogi the mouse falls back to its onboard profile.
    Software,
}

/// The effect one LED zone runs in an onboard profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardLighting {
    /// Dark.
    Off,
    /// One steady color.
    Fixed(Rgb),
    /// Cycling through the color wheel.
    Cycle {
        /// One full cycle, in milliseconds.
        period_ms: u16,
        /// Brightness, 1–100 percent.
        brightness: u8,
    },
    /// Fading one color in and out.
    Breathing {
        /// The color.
        color: Rgb,
        /// One full breath, in milliseconds.
        period_ms: u16,
        /// Peak brightness, 1–100 percent.
        brightness: u8,
    },
    /// An effect OpenLogi does not edit (set by other software). Saving a
    /// profile with this value leaves the zone's effect as it is.
    Unsupported,
}

/// The profile the mouse runs from its memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfile {
    /// The profile's 1-based number in its bank.
    pub number: u8,
    /// The name stored with it, if any.
    pub name: Option<String>,
    /// The mouse runs a read-only factory profile: its user memory holds
    /// none yet. Saving copies it into user memory first.
    pub factory: bool,
    /// The report rate, or `None` when the stored interval names none.
    pub report_rate: Option<ReportRate>,
    /// The DPI slots, in order; `None` marks an unused slot.
    pub dpi_slots: Vec<Option<Dpi>>,
    /// The slot the mouse starts in.
    pub default_dpi_slot: u8,
    /// One effect per LED zone; empty for a profile format without LEDs.
    pub lighting: Vec<OnboardLighting>,
}

/// What a mouse with onboard memory reports about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardMemory {
    /// The mode the mouse is in right now.
    pub mode: MemoryMode,
    /// The active profile, or `None` when the mouse declares a memory
    /// layout OpenLogi cannot decode — the mode still switches.
    pub profile: Option<OnboardProfile>,
}

/// The settings a save writes into the active onboard profile. Everything
/// else the profile holds — buttons, macros, name — is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileChange {
    /// The number of the profile the change was made against: a mouse that
    /// runs another one by now refuses it rather than overwrite that one.
    pub profile: u8,
    /// The DPI slots, in order; `None` clears a slot. At most as many as the
    /// profile holds.
    pub dpi_slots: Vec<Option<Dpi>>,
    /// The slot the mouse starts in; it must hold a DPI.
    pub default_dpi_slot: u8,
    /// The report rate.
    pub report_rate: ReportRate,
    /// One effect per LED zone, in zone order.
    pub lighting: Vec<OnboardLighting>,
}

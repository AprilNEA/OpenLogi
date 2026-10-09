//! Versioned gaming editor messages. The opaque backup uses the portable JSON
//! file schema; the desktop never constructs or decodes firmware bytes.

use serde::{Deserialize, Serialize};

/// A readback of the supported mouse's onboard memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamingSnapshot {
    /// Complete portable backup, returned unchanged as the edit precondition.
    pub backup_json: String,
    /// Execution mode: 1 onboard, 2 host.
    pub mode: u8,
    /// Active one-based profile sector.
    pub active_profile: u16,
    /// Profiles in firmware directory order.
    pub profiles: Vec<GamingProfile>,
    /// Durable local backup/edit file written by the agent.
    pub saved_path: Option<String>,
}

/// Presentation values decoded and validated by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamingProfile {
    pub sector: u16,
    pub enabled: bool,
    pub checksum_valid: bool,
    pub name: String,
    pub report_interval_ms: u8,
    pub dpi: [u16; 5],
    pub default_dpi_slot: u8,
    pub shift_dpi_slot: u8,
    pub buttons: Vec<String>,
    pub shifted_buttons: Vec<String>,
}

/// Firmware action chosen in the editor. Variant order is wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GamingAction {
    Mouse(u8),
    Key { usage: u8, modifiers: u8 },
    Consumer(u16),
    Special(u8),
    Disabled,
}

/// One edited binding; untouched bindings are omitted and preserved verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamingAssignment {
    pub index: u8,
    pub shifted: bool,
    pub action: GamingAction,
}

/// Draft values; validation and byte encoding belong to the device layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GamingDraft {
    pub sector: u16,
    pub name: String,
    pub report_rate_hz: u16,
    pub dpi: [u16; 5],
    pub default_dpi_slot: u8,
    pub shift_dpi_slot: u8,
    pub buttons: Vec<GamingAssignment>,
}

/// User-initiated operations. No operation implicitly changes execution mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GamingCommand {
    Read,
    Export,
    Prepare {
        backup_json: String,
        draft: GamingDraft,
    },
    Apply {
        backup_json: String,
        draft: GamingDraft,
    },
    SetMode(u8),
    Select(u16),
    /// Restore the original bytes only if memory still matches this applied draft.
    Restore {
        backup_json: String,
        draft: GamingDraft,
    },
}

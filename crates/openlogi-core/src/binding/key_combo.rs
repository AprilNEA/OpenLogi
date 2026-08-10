//! Modifier + virtual-key chords for custom shortcuts and workflows.

use serde::{Deserialize, Serialize};

/// A modifier + virtual-key keystroke captured by the P1.3 recorder UI or
/// hand-authored in `config.toml`.
///
/// `modifiers` is a bitmask of [`KeyCombo::MOD_CMD`] etc. so the wire format
/// is a compact integer, not a string. `key_code` is the macOS virtual key
/// (`kVK_*`); on Linux, `openlogi-inject` maps it to an evdev `KeyCode` when it
/// synthesizes the chord.
///
/// `display` is purely for rendering — e.g. `"⌘⇧P"`. Callers regenerate it
/// from the captured chord; we keep it in the struct so older configs
/// continue to render the same label without re-deriving on every load.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    /// Bitmask of [`Self::MOD_CMD`] etc.
    pub modifiers: u8,
    /// macOS virtual key code (`kVK_*`). 0 means "no key" — useful for
    /// modifier-only placeholders that the recorder UI rejects. On Linux,
    /// `openlogi-inject` translates this to an evdev `KeyCode`.
    pub key_code: u16,
    /// Pre-rendered chord label, e.g. `"⌘⇧P"`. Empty falls through to a
    /// generated label at runtime.
    #[serde(default)]
    pub display: String,
}

impl KeyCombo {
    /// Bit for the ⌘ Command modifier in [`Self::modifiers`].
    pub const MOD_CMD: u8 = 1 << 0;
    /// Bit for the ⇧ Shift modifier in [`Self::modifiers`].
    pub const MOD_SHIFT: u8 = 1 << 1;
    /// Bit for the ⌃ Control modifier in [`Self::modifiers`].
    pub const MOD_CTRL: u8 = 1 << 2;
    /// Bit for the ⌥ Option/Alt modifier in [`Self::modifiers`].
    pub const MOD_OPTION: u8 = 1 << 3;

    /// Build the human-readable label from the modifier bitmask + key code.
    /// Falls back to `"⌘key 0xNN"` when the key code isn't one of the
    /// commonly-recognised letters; the recorder UI usually overrides this
    /// with its own derivation.
    #[must_use]
    pub fn rendered_label(&self) -> String {
        if !self.display.is_empty() {
            return self.display.clone();
        }
        let mut out = String::new();
        if self.modifiers & Self::MOD_CTRL != 0 {
            out.push('⌃');
        }
        if self.modifiers & Self::MOD_OPTION != 0 {
            out.push('⌥');
        }
        if self.modifiers & Self::MOD_SHIFT != 0 {
            out.push('⇧');
        }
        if self.modifiers & Self::MOD_CMD != 0 {
            out.push('⌘');
        }
        match self.key_code {
            0x00 => out.push('A'),
            0x01 => out.push('S'),
            0x02 => out.push('D'),
            0x03 => out.push('F'),
            0x06 => out.push('Z'),
            0x07 => out.push('X'),
            0x08 => out.push('C'),
            0x09 => out.push('V'),
            0x0B => out.push('B'),
            0x0C => out.push('Q'),
            0x0D => out.push('W'),
            0x0E => out.push('E'),
            0x0F => out.push('R'),
            0x10 => out.push('Y'),
            0x11 => out.push('T'),
            0x20 => out.push('U'),
            0x22 => out.push('I'),
            0x1F => out.push('O'),
            0x23 => out.push('P'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "key 0x{:02X}", self.key_code);
            }
        }
        out
    }
}

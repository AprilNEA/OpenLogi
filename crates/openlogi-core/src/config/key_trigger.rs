//! Keyboard key triggers and the global keyboard-bindings section.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::binding::Action;

/// Detectable modifier state for a keyboard trigger. A leaf-level duplicate of
/// `openlogi_hook::KeyModifiers` — core must not depend on hook, so the four
/// bools are mirrored here and converted at the agent boundary (which depends
/// on both crates). `Fn` is absent: firmware-internal, unusable as a trigger
/// (function-key-remapper spec, Appendix A).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent modifier flags mirrored from the OS hook"
)]
pub struct KeyModifiers {
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub control: bool,
    /// Option/Alt held.
    pub option: bool,
    /// Command held.
    pub command: bool,
}

impl KeyModifiers {
    /// True when no modifiers are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.shift && !self.control && !self.option && !self.command
    }
}

/// A keyboard trigger: a keycode plus an optional modifier mask. The parse
/// format is `[mod+]+key`, e.g. `"f1"`, `"shift+cmd+f5"`. Modifier names:
/// `shift`, `control` (alias `ctrl`), `option` (alias `alt`), `command`
/// (alias `cmd`). Key names: `esc`, `f1`..`f19` (macOS virtual keycodes).
///
/// Serializes as its string form (via `Display`) so it can be a TOML map key:
/// `[keyboard.bindings]` keys are `"f1"`, `"shift+f2"`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyTrigger {
    /// Platform virtual keycode (macOS `kVK_*`).
    pub keycode: u16,
    /// Modifier mask that must also be held.
    pub modifiers: KeyModifiers,
}

impl std::fmt::Display for KeyTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let modifiers = &self.modifiers;
        let mut separator = "";
        for (enabled, name) in [
            (modifiers.shift, "shift"),
            (modifiers.control, "control"),
            (modifiers.option, "option"),
            (modifiers.command, "command"),
        ] {
            if enabled {
                write!(f, "{separator}{name}")?;
                separator = "+";
            }
        }
        write!(
            f,
            "{separator}{}",
            keycode_to_name(self.keycode).ok_or(std::fmt::Error)?
        )
    }
}

/// Reverse lookup for the parse table — needed so `Display` can render a
/// parsed trigger back to its canonical name.
fn keycode_to_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x35 => "esc",
        0x7A => "f1",
        0x78 => "f2",
        0x63 => "f3",
        0x76 => "f4",
        0x60 => "f5",
        0x61 => "f6",
        0x62 => "f7",
        0x64 => "f8",
        0x65 => "f9",
        0x6D => "f10",
        0x67 => "f11",
        0x6F => "f12",
        0x69 => "f13",
        0x6B => "f14",
        0x71 => "f15",
        0x6A => "f16",
        0x40 => "f17",
        0x4F => "f18",
        0x50 => "f19",
        _ => return None,
    })
}

// String-form serde so KeyTrigger can be a TOML map key.
impl Serialize for KeyTrigger {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for KeyTrigger {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Error returned by [`KeyTrigger`]'s `FromStr` impl.
#[derive(Debug, Error)]
#[error("invalid key trigger: {0}")]
pub struct ParseTriggerError(pub String);

impl std::str::FromStr for KeyTrigger {
    type Err = ParseTriggerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = KeyModifiers::default();
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
            return Err(ParseTriggerError("empty segment".into()));
        }
        // All but the last segment must be modifiers; the last is the key.
        let (mod_parts, key_part) = parts.split_at(parts.len() - 1);
        for part in mod_parts {
            match part.to_ascii_lowercase().as_str() {
                "shift" => mods.shift = true,
                "control" | "ctrl" => mods.control = true,
                "option" | "alt" => mods.option = true,
                "command" | "cmd" => mods.command = true,
                other => return Err(ParseTriggerError(format!("unknown modifier '{other}'"))),
            }
        }
        let keycode = match key_part[0].to_ascii_lowercase().as_str() {
            "esc" => 0x35,
            "f1" => 0x7A,
            "f2" => 0x78,
            "f3" => 0x63,
            "f4" => 0x76,
            "f5" => 0x60,
            "f6" => 0x61,
            "f7" => 0x62,
            "f8" => 0x64,
            "f9" => 0x65,
            "f10" => 0x6D,
            "f11" => 0x67,
            "f12" => 0x6F,
            "f13" => 0x69,
            "f14" => 0x6B,
            "f15" => 0x71,
            "f16" => 0x6A,
            "f17" => 0x40,
            "f18" => 0x4F,
            "f19" => 0x50,
            other => return Err(ParseTriggerError(format!("unknown key '{other}'"))),
        };
        Ok(KeyTrigger {
            keycode,
            modifiers: mods,
        })
    }
}

/// The top-level `[keyboard]` table. Bindings are keyed by [`KeyTrigger`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardConfig {
    /// Function-key trigger → action map for the remapper.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<KeyTrigger, Action>,
}

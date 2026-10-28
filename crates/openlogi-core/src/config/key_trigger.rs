//! Keyboard key triggers and the global keyboard-bindings section.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::FunctionKey;
use crate::binding::Action;

/// Detectable modifier state: what a keyboard trigger requires, and what the
/// OS hook reports with each key event (`openlogi-hook` re-exports this type).
/// `Fn` is absent: firmware-internal, never reported on non-function-row keys,
/// and so unusable as a trigger (function-key-remapper spec, Appendix A).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent modifier flags, as the OS reports them"
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
/// (alias `cmd`). Key names are [`FunctionKey`]'s: `esc`, `f1`..`f19`.
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
        let key = FunctionKey::from_keycode(self.keycode).ok_or(std::fmt::Error)?;
        write!(f, "{separator}{}", key.name())
    }
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
        let key = key_part[0].to_ascii_lowercase();
        let keycode = FunctionKey::from_name(&key)
            .ok_or_else(|| ParseTriggerError(format!("unknown key '{key}'")))?
            .keycode();
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

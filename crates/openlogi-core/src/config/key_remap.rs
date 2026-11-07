//! Firmware key remapping for keyboards with HID++ `0x1b05` key
//! customisation (the G515 LS TKL): what a key sends instead of its own
//! usage, on each of the keyboard's three layers.
//!
//! Keys and keyboard targets reuse the [`LightingKey`] names, which say where
//! a key sits rather than what its keycap prints.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::LightingKey;

/// What each remapped key sends, per layer. A key a layer leaves out keeps
/// the keyboard's own behaviour there.
///
/// The config reads
/// `key_remap = { base = { CapsLock = "Esc" }, fn = { F1 = "mute" } }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRemap {
    /// Keys pressed on their own.
    #[serde(
        default,
        deserialize_with = "deserialize_layer",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub base: BTreeMap<LightingKey, KeyTarget>,
    /// Keys pressed while Fn is held.
    #[serde(
        default,
        rename = "fn",
        deserialize_with = "deserialize_layer",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub fn_layer: BTreeMap<LightingKey, KeyTarget>,
    /// Keys pressed while a key remapped to [`KeyTarget::GShift`] is held.
    #[serde(
        default,
        deserialize_with = "deserialize_layer",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub g_shift: BTreeMap<LightingKey, KeyTarget>,
}

impl KeyRemap {
    /// Whether no layer remaps anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.fn_layer.is_empty() && self.g_shift.is_empty()
    }

    /// The three layers in the keyboard's order: base, Fn, G-Shift.
    #[must_use]
    pub fn layers(&self) -> [&BTreeMap<LightingKey, KeyTarget>; 3] {
        [&self.base, &self.fn_layer, &self.g_shift]
    }
}

/// What a remapped key sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum KeyTarget {
    /// Another keyboard key.
    Key(LightingKey),
    /// A media key.
    Media(MediaKey),
    /// The G-Shift modifier: while held, keys send their
    /// [`KeyRemap::g_shift`] targets.
    GShift,
}

/// The media keys a key can be remapped to, by USB HID consumer usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaKey {
    /// Mute.
    Mute,
    /// Volume up.
    VolumeUp,
    /// Volume down.
    VolumeDown,
    /// Play/pause.
    PlayPause,
    /// Next track.
    NextTrack,
    /// Previous track.
    PreviousTrack,
}

/// Media key names in the config and their consumer usages.
const MEDIA: &[(&str, MediaKey, u16)] = &[
    ("mute", MediaKey::Mute, 0x00e2),
    ("volume_up", MediaKey::VolumeUp, 0x00e9),
    ("volume_down", MediaKey::VolumeDown, 0x00ea),
    ("play_pause", MediaKey::PlayPause, 0x00cd),
    ("next_track", MediaKey::NextTrack, 0x00b5),
    ("previous_track", MediaKey::PreviousTrack, 0x00b6),
];

/// The config name of [`KeyTarget::GShift`].
const G_SHIFT: &str = "g_shift";

impl MediaKey {
    /// The config name, such as `volume_up`.
    #[must_use]
    pub fn name(self) -> &'static str {
        MEDIA
            .iter()
            .find(|(_, key, _)| *key == self)
            .map_or("?", |(name, _, _)| name)
    }

    /// The USB HID consumer usage the key sends.
    #[must_use]
    pub fn usage(self) -> u16 {
        MEDIA
            .iter()
            .find(|(_, key, _)| *key == self)
            .map_or(0, |(_, _, usage)| *usage)
    }

    /// Every media key.
    pub fn all() -> impl Iterator<Item = Self> {
        MEDIA.iter().map(|(_, key, _)| *key)
    }
}

/// A target name that is neither a key, a media key nor `g_shift`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "unknown key target {input:?} (expected a key name such as Esc or F1, a media key such as \
     mute or volume_up, or g_shift)"
)]
pub struct KeyTargetError {
    input: String,
}

impl FromStr for KeyTarget {
    type Err = KeyTargetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case(G_SHIFT) {
            return Ok(Self::GShift);
        }
        if let Some((_, media, _)) = MEDIA
            .iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(s))
        {
            return Ok(Self::Media(*media));
        }
        s.parse::<LightingKey>()
            .map(Self::Key)
            .map_err(|_| KeyTargetError { input: s.into() })
    }
}

impl TryFrom<String> for KeyTarget {
    type Error = KeyTargetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<KeyTarget> for String {
    fn from(target: KeyTarget) -> Self {
        target.to_string()
    }
}

impl fmt::Display for KeyTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => f.write_str(key.name()),
            Self::Media(media) => f.write_str(media.name()),
            Self::GShift => f.write_str(G_SHIFT),
        }
    }
}

/// A layer's key names ignore case, so two spellings of one key are an error
/// rather than one silently replacing the other.
fn deserialize_layer<'de, D>(deserializer: D) -> Result<BTreeMap<LightingKey, KeyTarget>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LayerVisitor;

    impl<'de> serde::de::Visitor<'de> for LayerVisitor {
        type Value = BTreeMap<LightingKey, KeyTarget>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a table of key targets keyed by key name")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            use serde::de::Error as _;

            let mut layer = BTreeMap::new();
            let mut spellings: BTreeMap<LightingKey, String> = BTreeMap::new();
            while let Some(name) = access.next_key::<String>()? {
                let key: LightingKey = name.parse().map_err(A::Error::custom)?;
                let target = access.next_value::<String>()?;
                let target: KeyTarget = target
                    .parse()
                    .map_err(|error| A::Error::custom(format_args!("key {key}: {error}")))?;
                if let Some(earlier) = spellings.insert(key, name.clone()) {
                    return Err(A::Error::custom(format_args!(
                        "`{earlier}` and `{name}` name the same key; keep one"
                    )));
                }
                layer.insert(key, target);
            }
            Ok(layer)
        }
    }

    deserializer.deserialize_map(LayerVisitor)
}

#[cfg(test)]
mod tests {
    use super::{KeyRemap, KeyTarget, MediaKey};
    use crate::config::LightingKey;

    fn key(name: &str) -> LightingKey {
        name.parse().expect("a key name")
    }

    #[test]
    fn targets_parse_keys_media_and_g_shift_in_any_case() {
        assert_eq!("esc".parse(), Ok(KeyTarget::Key(key("Esc"))));
        assert_eq!(
            "Volume_Up".parse(),
            Ok(KeyTarget::Media(MediaKey::VolumeUp))
        );
        assert_eq!("G_SHIFT".parse(), Ok(KeyTarget::GShift));
        "volume".parse::<KeyTarget>().expect_err("not a target");
        for target in [
            KeyTarget::Key(key("F1")),
            KeyTarget::Media(MediaKey::Mute),
            KeyTarget::GShift,
        ] {
            assert_eq!(target.to_string().parse(), Ok(target));
        }
    }

    #[test]
    fn media_keys_send_their_consumer_usages() {
        assert_eq!(MediaKey::Mute.usage(), 0xe2);
        assert_eq!(MediaKey::VolumeDown.usage(), 0xea);
        assert_eq!(MediaKey::VolumeUp.usage(), 0xe9);
        assert!(MediaKey::all().all(|media| media.usage() != 0));
    }

    #[test]
    fn a_remap_round_trips_through_toml_with_layers_named() {
        let toml = r#"
            base = { CapsLock = "Esc", Menu = "g_shift" }
            fn = { F1 = "mute" }
            g_shift = { F1 = "D" }
        "#;
        let remap: KeyRemap = toml::from_str(toml).expect("parses");
        assert_eq!(remap.base[&key("CapsLock")], KeyTarget::Key(key("Esc")));
        assert_eq!(remap.base[&key("Menu")], KeyTarget::GShift);
        assert_eq!(remap.fn_layer[&key("F1")], KeyTarget::Media(MediaKey::Mute));
        assert_eq!(remap.g_shift[&key("F1")], KeyTarget::Key(key("D")));

        let saved = toml::to_string(&remap).expect("serializes");
        assert_eq!(
            toml::from_str::<KeyRemap>(&saved).expect("reads back"),
            remap
        );
        assert!(saved.contains("[fn]") || saved.contains("fn ="), "{saved}");
        assert!(
            !KeyRemap::default()
                .layers()
                .iter()
                .any(|layer| !layer.is_empty())
        );
    }

    #[test]
    fn two_spellings_of_one_key_are_refused() {
        let error = toml::from_str::<KeyRemap>(r#"base = { Esc = "A", esc = "B" }"#)
            .expect_err("one key twice");
        assert!(error.to_string().contains("name the same key"), "{error}");
        toml::from_str::<KeyRemap>(r#"base = { Esc = "Fn" }"#).expect_err("no such target");
        toml::from_str::<KeyRemap>("layer4 = {}").expect_err("no such layer");
    }
}

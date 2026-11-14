//! How a keypad's LCD key looks beyond its action's icon: a label, colours
//! and a picture the user chose.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::color::Rgb;

/// The user's styling of one LCD key on one binding page.
///
/// Stored per grid slot in a device's `key_faces` table
/// (`[devices."…".key_faces.gridTopLeft]`, `page2:gridTopLeft` on a later
/// page). A key without an entry shows its action's icon, white on black.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyFaceStyle {
    /// Text drawn on the key. `\n` breaks a line; at most two lines are
    /// shown, sized to fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Tile background; black when unset. A `gradient` replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgb>,
    /// Label colour, and the icon's too unless `icon_color` sets its own;
    /// white when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgb>,
    /// A PNG or JPEG file drawn under the icon and label, cropped to fill the
    /// key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<PathBuf>,
    /// Whether the action's icon is drawn. On unless turned off, so a label
    /// alone keeps the icon above it.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub icon: bool,
    /// A symbol drawn in place of the action's icon: a Lucide icon by name, or
    /// an emoji. Shown whatever `icon` says — choosing one is the request to
    /// show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<KeySymbol>,
    /// Colour of the action icon or Lucide symbol, when it should differ from
    /// the label's `color`. An emoji keeps its own colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_color: Option<Rgb>,
    /// A two-colour gradient drawn as the background instead of the flat
    /// `background`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<KeyGradient>,
}

/// Whether a key picture at `path` may be touched at all — looked up, read,
/// shown. A network share or device path is not: a stat or a read could hang
/// on one that is slow or gone, so the agent never reads it and the settings
/// app never shows or accepts it. Decided from the path alone, with no file
/// access, so it is safe anywhere — a render included.
///
/// A path without a Windows prefix is always local by this rule; on other
/// platforms a mounted share is just a directory.
#[must_use]
pub fn is_local_picture_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    !matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(
            prefix.kind(),
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..) | Prefix::Verbatim(..)
        )
    )
}

/// What tells two versions of one picture file apart without reading it: its
/// size and when it was last written. `None` stands for what the platform
/// cannot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileRevision {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

/// The versions of the pictures on keys, as last read from disk.
///
/// Reading a version is a file-system call, which can stall on a slow or
/// unavailable share, so it happens in [`Self::read`], off the threads that
/// draw or hold a lock; everything else only looks a version up. The agent
/// redraws a key, and the settings app reloads its preview, when a picture's
/// version changes — a file replaced at the same path saves no config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PictureRevisions(std::collections::BTreeMap<PathBuf, Option<FileRevision>>);

impl PictureRevisions {
    /// Read the version of each picture in `paths`. Blocking: run it off the
    /// async and UI threads, and never under a lock other work waits on. A
    /// path [`is_local_picture_path`] rejects is never touched, and a file
    /// that cannot be read has no version, so its becoming readable is a
    /// change.
    #[cfg(feature = "fs")]
    #[must_use]
    pub fn read(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self(
            paths
                .into_iter()
                .map(|path| {
                    let revision = is_local_picture_path(&path)
                        .then(|| std::fs::metadata(&path).ok())
                        .flatten()
                        .map(|meta| FileRevision {
                            modified: meta.modified().ok(),
                            len: meta.len(),
                        });
                    (path, revision)
                })
                .collect(),
        )
    }

    /// The version of `path` last read: `None` while it has not been read, or
    /// could not be.
    #[must_use]
    pub fn revision(&self, path: &Path) -> Option<FileRevision> {
        self.0.get(path).copied().flatten()
    }

    /// The pictures `newer` read whose version is not the one `self` read —
    /// those to draw again. A picture `self` never read counts too: it may
    /// have been drawn before any version was read, and replaced since, so
    /// nothing says the version drawn is the one `newer` read.
    pub fn changed<'a>(&'a self, newer: &'a Self) -> impl Iterator<Item = &'a Path> {
        newer
            .0
            .iter()
            .filter(|(path, revision)| self.0.get(*path) != Some(*revision))
            .map(|(path, _)| path.as_path())
    }
}

/// A linear two-colour background gradient.
///
/// Written `gradient = { from = "ff5a00", to = "7800c8", direction = "diagonal" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyGradient {
    /// The colour at the start of the gradient.
    pub from: Rgb,
    /// The colour at its end.
    pub to: Rgb,
    /// Which way it runs; top to bottom when unset.
    #[serde(default, skip_serializing_if = "GradientDirection::is_default")]
    pub direction: GradientDirection,
}

/// Which way a [`KeyGradient`] runs across the key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GradientDirection {
    /// Top to bottom.
    #[default]
    Vertical,
    /// Left to right.
    Horizontal,
    /// Top left to bottom right.
    Diagonal,
}

impl GradientDirection {
    /// Every direction, in the order a picker offers them.
    pub const ALL: [Self; 3] = [Self::Vertical, Self::Horizontal, Self::Diagonal];

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes a reference"
    )]
    fn is_default(&self) -> bool {
        *self == Self::Vertical
    }
}

/// A symbol a keypad key can show instead of its action's icon.
///
/// Written `symbol = { lucide = "camera" }` or `symbol = { emoji = "🎨" }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum KeySymbol {
    /// A [Lucide](https://lucide.dev) icon by its kebab-case name, drawn in
    /// the key's text colour.
    Lucide(String),
    /// An emoji (or any short text), drawn in its own colours.
    Emoji(String),
}

impl KeySymbol {
    /// Longest emoji text kept, in characters: a family or flag sequence
    /// joins several code points, but a key shows one symbol.
    pub const MAX_EMOJI_CHARS: usize = 16;

    /// This symbol with surrounding whitespace trimmed, or `None` when
    /// nothing is left to draw. An over-long emoji is cut to
    /// [`Self::MAX_EMOJI_CHARS`].
    #[must_use]
    pub fn normalized(&self) -> Option<Self> {
        match self {
            Self::Lucide(name) => {
                let name = name.trim().to_ascii_lowercase();
                (!name.is_empty()).then_some(Self::Lucide(name))
            }
            Self::Emoji(text) => {
                let text: String = text.trim().chars().take(Self::MAX_EMOJI_CHARS).collect();
                (!text.is_empty()).then_some(Self::Emoji(text))
            }
        }
    }
}

impl Default for KeyFaceStyle {
    fn default() -> Self {
        Self {
            label: None,
            background: None,
            color: None,
            image: None,
            icon: true,
            symbol: None,
            icon_color: None,
            gradient: None,
        }
    }
}

impl KeyFaceStyle {
    /// Whether this style changes nothing about the default face, so the entry
    /// can be dropped.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The label with surrounding whitespace removed, or `None` when it is
    /// empty.
    #[must_use]
    pub fn trimmed_label(&self) -> Option<&str> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
    }
}

const fn default_true() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_true(value: &bool) -> bool {
    *value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_style_roundtrips_and_the_default_icon_stays_out_of_the_file() {
        let style = KeyFaceStyle {
            label: Some("Export\nPNG".into()),
            background: Some(Rgb::new(0x20, 0x40, 0x80)),
            color: None,
            image: Some(PathBuf::from("C:/icons/export.png")),
            icon: true,
            symbol: Some(KeySymbol::Lucide("camera".into())),
            icon_color: Some(Rgb::new(0xff, 0xcc, 0x00)),
            gradient: Some(KeyGradient {
                from: Rgb::new(0xff, 0x5a, 0x00),
                to: Rgb::new(0x78, 0x00, 0xc8),
                direction: GradientDirection::Diagonal,
            }),
        };
        let toml = toml::to_string(&style).unwrap();
        assert!(
            !toml.lines().any(|line| line.starts_with("icon =")),
            "{toml}"
        );
        assert_eq!(toml::from_str::<KeyFaceStyle>(&toml).unwrap(), style);
        let emoji: KeyFaceStyle = toml::from_str("symbol = { emoji = \"🎨\" }").unwrap();
        assert_eq!(emoji.symbol, Some(KeySymbol::Emoji("🎨".into())));
        assert_eq!(
            KeySymbol::Lucide(" Camera ".into()).normalized(),
            Some(KeySymbol::Lucide("camera".into()))
        );
        assert_eq!(KeySymbol::Emoji("  ".into()).normalized(), None);
        let vertical: KeyFaceStyle =
            toml::from_str(r#"gradient = { from = "000000", to = "ffffff" }"#).unwrap();
        assert_eq!(
            vertical.gradient.map(|g| g.direction),
            Some(GradientDirection::Vertical)
        );
        assert!(
            !toml::to_string(&vertical).unwrap().contains("direction"),
            "the default direction stays out of the file"
        );
        let hidden: KeyFaceStyle = toml::from_str("icon = false").unwrap();
        assert!(!hidden.icon);
        assert!(KeyFaceStyle::default().is_default());
        assert_eq!(
            KeyFaceStyle {
                label: Some("  ".into()),
                ..KeyFaceStyle::default()
            }
            .trimmed_label(),
            None
        );
    }
}

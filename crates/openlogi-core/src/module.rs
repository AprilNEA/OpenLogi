//! Key modules: separate programs that own keypad LCD keys.
//!
//! A module lives in its own folder under the modules directory, described by
//! a `module.toml` [manifest](ModuleManifest). A key bound to
//! [`Action::Module`](crate::binding::Action::Module) shows the images that
//! module sends and delivers its presses to it; the agent runs the module as
//! a child process and speaks the line protocol `docs/MODULES.md` describes.
//! This module holds the pure half: the identifier and the manifest. Running
//! modules is the agent's.

use std::path::{Path, PathBuf};

use nutype::nutype;
use serde::Deserialize;
use thiserror::Error;

use crate::app::AppPlatform;

/// The manifest's file name inside a module folder.
pub const MANIFEST_FILE: &str = "module.toml";

/// The version of the module protocol this build speaks. A manifest naming
/// another is refused, so the module never runs against a host that would
/// misread it.
pub const PROTOCOL: u32 = 1;

/// The environment variable a running module finds its own id in.
pub const ENV_MODULE_ID: &str = "OPENLOGI_MODULE_ID";

/// The environment variable a running module finds its folder in; also its
/// working directory.
pub const ENV_MODULE_DIR: &str = "OPENLOGI_MODULE_DIR";

/// The longest [`ModuleId`], in bytes.
const MAX_ID_LEN: usize = 128;

/// A module's identifier: reverse-DNS, such as `com.example.spotify`.
///
/// Lower-case ASCII letters, digits, `.` and `-`, at least two dot-separated
/// labels, none empty and none starting or ending with `-`, at most 128
/// bytes. Persisted verbatim in `config.toml`, both as the payload of
/// [`Action::Module`](crate::binding::Action::Module) and as the key of the
/// module's `[modules."<id>"]` table.
#[nutype(
    validate(with = validate_module_id, error = ModuleIdError),
    derive(
        Clone,
        Debug,
        Display,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Hash,
        AsRef,
        TryFrom,
        Serialize,
        Deserialize
    )
)]
pub struct ModuleId(String);

/// Why a string is not a [`ModuleId`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ModuleIdError {
    /// The identifier is empty or longer than 128 bytes.
    #[error("a module id must be 1 to {MAX_ID_LEN} characters long")]
    Length,
    /// The identifier has a character outside `a-z`, `0-9`, `.` and `-`.
    #[error("module id {0:?} can contain only a-z, 0-9, `.` and `-`")]
    InvalidCharacter(String),
    /// The identifier is not dot-separated labels, at least two, each
    /// non-empty and neither starting nor ending with `-`.
    #[error("module id {0:?} is not a reverse-DNS name such as com.example.module")]
    NotReverseDns(String),
}

fn validate_module_id(id: &str) -> Result<(), ModuleIdError> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(ModuleIdError::Length);
    }
    if !id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
    }) {
        return Err(ModuleIdError::InvalidCharacter(id.to_owned()));
    }
    let mut labels = 0;
    for label in id.split('.') {
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') {
            return Err(ModuleIdError::NotReverseDns(id.to_owned()));
        }
        labels += 1;
    }
    if labels < 2 {
        return Err(ModuleIdError::NotReverseDns(id.to_owned()));
    }
    Ok(())
}

/// A module's `module.toml`, validated. See `docs/MODULES.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleManifest {
    id: ModuleId,
    name: String,
    description: Option<String>,
    version: Option<String>,
    platforms: Vec<AppPlatform>,
    executable: String,
}

/// Why a `module.toml` was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ManifestError {
    /// The file is not TOML of the manifest's shape: a syntax error, a
    /// missing required key, a key of the wrong type, or an unknown key.
    #[error("module.toml is invalid: {0}")]
    Toml(String),
    /// `id` is not a valid [`ModuleId`].
    #[error("module.toml has an invalid id: {0}")]
    Id(#[from] ModuleIdError),
    /// `name` is empty.
    #[error("module.toml has an empty name")]
    EmptyName,
    /// `protocol` names a version this build does not speak.
    #[error("the module speaks protocol {found}; this OpenLogi speaks protocol {PROTOCOL}")]
    UnsupportedProtocol {
        /// The version the manifest names.
        found: u32,
    },
    /// `platforms` is empty.
    #[error("module.toml lists no platforms")]
    NoPlatforms,
    /// `platforms` names something other than `macos`, `linux` or `windows`.
    #[error("module.toml lists unknown platform {0:?}; use macos, linux or windows")]
    UnknownPlatform(String),
    /// `executable` is empty, absolute, or leaves the module folder.
    #[error(
        "module.toml executable {0:?} must be a path inside the module folder, not absolute and without `..`"
    )]
    Executable(String),
}

/// The manifest as written, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    protocol: u32,
    platforms: Vec<String>,
    executable: String,
}

impl ModuleManifest {
    /// Parse and validate the text of a `module.toml`.
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest =
            toml::from_str(source).map_err(|error| ManifestError::Toml(error.message().into()))?;
        let id = ModuleId::try_new(raw.id)?;
        if raw.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if raw.protocol != PROTOCOL {
            return Err(ManifestError::UnsupportedProtocol {
                found: raw.protocol,
            });
        }
        if raw.platforms.is_empty() {
            return Err(ManifestError::NoPlatforms);
        }
        let mut platforms = Vec::with_capacity(raw.platforms.len());
        for name in &raw.platforms {
            let platform = AppPlatform::ALL
                .into_iter()
                .find(|platform| platform.field() == name)
                .ok_or_else(|| ManifestError::UnknownPlatform(name.clone()))?;
            if !platforms.contains(&platform) {
                platforms.push(platform);
            }
        }
        if !is_contained_path(&raw.executable) {
            return Err(ManifestError::Executable(raw.executable));
        }
        Ok(Self {
            id,
            name: raw.name,
            description: raw.description.filter(|text| !text.trim().is_empty()),
            version: raw.version.filter(|text| !text.trim().is_empty()),
            platforms,
            executable: raw.executable,
        })
    }

    /// The module's identifier.
    #[must_use]
    pub fn id(&self) -> &ModuleId {
        &self.id
    }

    /// What the app calls the module.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The module's one-line description, when it has one.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The module's own version, shown only.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The platforms the module runs on, by their manifest names, in the
    /// order the manifest lists them.
    pub fn platforms(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.platforms.iter().map(|platform| platform.field())
    }

    /// Whether the module runs on the platform this build runs on.
    #[must_use]
    pub fn runs_here(&self) -> bool {
        self.platforms.contains(&AppPlatform::CURRENT)
    }

    /// The executable's path as the manifest writes it, relative to the
    /// module folder.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The executable inside the module folder `folder`.
    #[must_use]
    pub fn executable_in(&self, folder: &Path) -> PathBuf {
        folder.join(&self.executable)
    }
}

/// Whether `path` names a file inside the folder it is joined to on every
/// platform: not empty, not absolute in either path syntax, no drive or
/// stream colon, and no `..` component.
fn is_contained_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.contains(':')
        && path.split(['/', '\\']).all(|component| component != "..")
        && path
            .split(['/', '\\'])
            .any(|component| !component.is_empty() && component != ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPOTIFY: &str = r#"
id = "com.example.spotify"
name = "Spotify Now Playing"
description = "Album art and play/pause for Spotify."
version = "1.0.0"
protocol = 1
platforms = ["macos", "linux"]
executable = "bin/spotify-module"
"#;

    #[test]
    fn module_ids_are_reverse_dns_names() {
        for id in ["com.example.spotify", "a.b", "io.github.user-name.weather2"] {
            ModuleId::try_new(id.to_owned()).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        let refused = |id: &str| ModuleId::try_new(id.to_owned()).unwrap_err();
        assert_eq!(refused(""), ModuleIdError::Length);
        assert_eq!(
            refused(&format!("a.{}", "b".repeat(MAX_ID_LEN - 1))),
            ModuleIdError::Length
        );
        ModuleId::try_new(format!("a.{}", "b".repeat(MAX_ID_LEN - 2)))
            .expect("128 bytes is the most");
        for id in [
            "Com.Example",
            "com.example spotify",
            "com_example.x",
            "com.ex/ample",
        ] {
            assert!(
                matches!(refused(id), ModuleIdError::InvalidCharacter(_)),
                "{id}"
            );
        }
        for id in [
            "spotify",
            "com..example",
            ".com.example",
            "com.example.",
            "com.-x",
            "com.x-",
        ] {
            assert!(
                matches!(refused(id), ModuleIdError::NotReverseDns(_)),
                "{id}"
            );
        }
    }

    #[test]
    fn a_complete_manifest_parses() {
        let manifest = ModuleManifest::parse(SPOTIFY).unwrap();
        assert_eq!(manifest.id().as_ref(), "com.example.spotify");
        assert_eq!(manifest.name(), "Spotify Now Playing");
        assert_eq!(
            manifest.description(),
            Some("Album art and play/pause for Spotify.")
        );
        assert_eq!(manifest.version(), Some("1.0.0"));
        assert_eq!(manifest.platforms().collect::<Vec<_>>(), ["macos", "linux"]);
        assert_eq!(
            manifest.executable_in(Path::new("/m/spotify")),
            Path::new("/m/spotify/bin/spotify-module")
        );
        assert_eq!(
            manifest.runs_here(),
            !cfg!(target_os = "windows"),
            "listed for macOS and Linux only"
        );
    }

    #[test]
    fn description_and_version_are_optional() {
        let manifest = ModuleManifest::parse(
            r#"
id = "com.example.min"
name = "Min"
protocol = 1
platforms = ["macos", "linux", "windows", "linux"]
executable = "run"
"#,
        )
        .unwrap();
        assert_eq!(manifest.description(), None);
        assert_eq!(manifest.version(), None);
        assert_eq!(
            manifest.platforms().collect::<Vec<_>>(),
            ["macos", "linux", "windows"],
            "a repeated platform is listed once"
        );
        assert!(manifest.runs_here());
    }

    fn with(key: &str, value: &str) -> String {
        SPOTIFY
            .lines()
            .map(|line| {
                if line.starts_with(&format!("{key} =")) {
                    format!("{key} = {value}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn without(key: &str) -> String {
        SPOTIFY
            .lines()
            .filter(|line| !line.starts_with(&format!("{key} =")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_manifest_missing_a_required_key_is_refused() {
        for key in ["id", "name", "protocol", "platforms", "executable"] {
            assert!(
                matches!(
                    ModuleManifest::parse(&without(key)),
                    Err(ManifestError::Toml(_))
                ),
                "{key}"
            );
        }
    }

    #[test]
    fn unknown_keys_and_bad_values_are_refused() {
        let parse = |source: &str| ModuleManifest::parse(source).unwrap_err();
        assert!(matches!(
            parse(&format!("{SPOTIFY}\nentry = \"x\"\n")),
            ManifestError::Toml(_)
        ));
        assert!(matches!(parse("id = "), ManifestError::Toml(_)));
        assert!(matches!(
            parse(&with("id", "\"Spotify\"")),
            ManifestError::Id(_)
        ));
        assert_eq!(parse(&with("name", "\"  \"")), ManifestError::EmptyName);
        assert_eq!(
            parse(&with("protocol", "2")),
            ManifestError::UnsupportedProtocol { found: 2 }
        );
        assert!(matches!(
            parse(&with("protocol", "-1")),
            ManifestError::Toml(_)
        ));
        assert_eq!(parse(&with("platforms", "[]")), ManifestError::NoPlatforms);
        assert_eq!(
            parse(&with("platforms", "[\"macos\", \"bsd\"]")),
            ManifestError::UnknownPlatform("bsd".into())
        );
    }

    #[test]
    fn the_executable_stays_inside_the_module_folder() {
        for path in [
            "\"\"",
            "\".\"",
            "\"/usr/bin/env\"",
            "\"\\\\\\\\server\\\\share\\\\x.exe\"",
            "\"C:\\\\x.exe\"",
            "\"../other/run\"",
            "\"bin/../../run\"",
            "\"bin\\\\..\\\\..\\\\run.exe\"",
        ] {
            assert!(
                matches!(
                    ModuleManifest::parse(&with("executable", path)),
                    Err(ManifestError::Executable(_))
                ),
                "{path}"
            );
        }
        for path in [
            "\"run\"",
            "\"./run\"",
            "\"bin/run.exe\"",
            "\"bin\\\\run.exe\"",
        ] {
            ModuleManifest::parse(&with("executable", path))
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }
}

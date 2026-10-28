//! The application alias table: one name for an application across the
//! identifiers each platform reports for it.
//!
//! [`ForegroundApp::id`](super::ForegroundApp::id) is a different string on
//! each platform — a bundle identifier on macOS, a `WM_CLASS` or `app_id` on
//! Linux, an executable path on Windows — so a per-app profile keyed by one of
//! them stops applying when the same `config.toml` is used on another
//! platform. An alias lists one application's identifiers per platform, and a
//! profile keyed `app:<alias>` applies wherever one of them is in front.
//! [`overlay_for`](super::overlay_for) is the matcher that consults it.

use std::collections::BTreeMap;
use std::fmt;

use nutype::nutype;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeSeq as _, Serializer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The profile-key prefix that names an alias rather than a platform
/// identifier. No platform identifier starts with it: bundle identifiers and
/// Linux classes carry no colon, macOS paths start with `/`, and a Windows
/// path starts with a one-letter drive or `\\`.
const PROFILE_KEY_PREFIX: &str = "app:";

/// The name of an alias: the `<alias>` in `[apps.<alias>]` and in the
/// `app:<alias>` profile key.
///
/// Limited to the characters of a TOML bare key — ASCII letters, digits, `-`,
/// and `_` — so the table header never needs quoting and the name is spelled
/// the same in both places.
#[nutype(
    validate(with = validate_alias_name, error = AppAliasNameError),
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
pub struct AppAliasName(String);

impl AppAliasName {
    /// The per-app profile key that selects this alias: `app:<alias>`.
    #[must_use]
    pub fn profile_key(&self) -> String {
        format!("{PROFILE_KEY_PREFIX}{}", self.as_ref())
    }
}

/// Why a string is not an [`AppAliasName`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AppAliasNameError {
    /// The name is empty.
    #[error("an application alias name cannot be empty")]
    Empty,
    /// The name has a character a TOML bare key cannot carry.
    #[error("application alias name {0:?} can contain only ASCII letters, digits, `-`, and `_`")]
    InvalidCharacter(String),
}

fn validate_alias_name(name: &str) -> Result<(), AppAliasNameError> {
    if name.is_empty() {
        return Err(AppAliasNameError::Empty);
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppAliasNameError::InvalidCharacter(name.to_string()));
    }
    Ok(())
}

/// The platform whose identifiers an alias is matched by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppPlatform {
    MacOs,
    Linux,
    Windows,
}

impl AppPlatform {
    /// The platform this build runs on. Every other Unix reports applications
    /// the way Linux does, through X11 or Wayland.
    #[cfg(target_os = "macos")]
    pub(crate) const CURRENT: Self = Self::MacOs;
    /// The platform this build runs on.
    #[cfg(target_os = "windows")]
    pub(crate) const CURRENT: Self = Self::Windows;
    /// The platform this build runs on. Every other Unix reports applications
    /// the way Linux does, through X11 or Wayland.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub(crate) const CURRENT: Self = Self::Linux;

    pub(crate) const ALL: [Self; 3] = [Self::MacOs, Self::Linux, Self::Windows];

    /// The `[apps.<alias>]` field this platform's identifiers live under.
    pub(crate) const fn field(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}

/// One platform's identifiers for an alias. `config.toml` writes a single
/// identifier as a string and several as an array.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Identifiers(Vec<String>);

impl Identifiers {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Identifiers {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let [only] = self.0.as_slice() {
            return serializer.serialize_str(only);
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for identifier in &self.0 {
            seq.serialize_element(identifier)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Identifiers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OneOrMany;

        impl<'de> Visitor<'de> for OneOrMany {
            type Value = Identifiers;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an application identifier or an array of them")
            }

            fn visit_str<E: de::Error>(self, identifier: &str) -> Result<Self::Value, E> {
                Ok(Identifiers(vec![identifier.to_string()]))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut identifiers = Vec::with_capacity(seq.size_hint().unwrap_or_default());
                while let Some(identifier) = seq.next_element::<String>()? {
                    identifiers.push(identifier);
                }
                Ok(Identifiers(identifiers))
            }
        }

        deserializer.deserialize_any(OneOrMany)
    }
}

/// One application as each platform identifies it: an `[apps.<alias>]` table.
///
/// Each platform field takes the identifiers [`ForegroundApp::id`] reports
/// there, in the forms a per-app profile key accepts — including the Windows
/// `exe:<filename>` selector. Several identifiers cover an application that
/// installs in more than one place or renames itself between versions.
///
/// [`ForegroundApp::id`]: super::ForegroundApp::id
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppAlias {
    /// What a UI calls the application, such as `Google Chrome`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Bundle identifiers, or executable paths for applications without one.
    #[serde(default, skip_serializing_if = "Identifiers::is_empty")]
    macos: Identifiers,
    /// X11 `WM_CLASS` classes or Wayland `app_id`s.
    #[serde(default, skip_serializing_if = "Identifiers::is_empty")]
    linux: Identifiers,
    /// Lower-case executable paths or `exe:<filename>` selectors.
    #[serde(default, skip_serializing_if = "Identifiers::is_empty")]
    windows: Identifiers,
}

impl AppAlias {
    /// What a UI calls the application, or `None` when the alias has no
    /// (non-empty) `name`.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().filter(|name| !name.trim().is_empty())
    }

    /// The identifiers this alias lists for the platform OpenLogi runs on, in
    /// the order the file lists them. Empty when the alias does not name the
    /// application here, so its profiles do not apply on this platform.
    #[must_use]
    pub fn identifiers_here(&self) -> &[String] {
        self.identifiers(AppPlatform::CURRENT)
    }

    fn identifiers(&self, platform: AppPlatform) -> &[String] {
        match platform {
            AppPlatform::MacOs => &self.macos.0,
            AppPlatform::Linux => &self.linux.0,
            AppPlatform::Windows => &self.windows.0,
        }
    }
}

/// The `[apps]` table: every application alias, by name.
///
/// Always valid: deserializing checks every identifier and that no identifier
/// belongs to two aliases on the same platform, so one foreground application
/// resolves to at most one alias.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppAliases(BTreeMap<AppAliasName, AppAlias>);

/// Why an `[apps]` table is invalid.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AppAliasError {
    /// An identifier is the empty string.
    #[error("application alias {alias} lists an empty {platform} identifier")]
    EmptyIdentifier {
        /// The alias listing it.
        alias: AppAliasName,
        /// The field it is listed under.
        platform: &'static str,
    },
    /// An identifier names an alias instead of an application.
    #[error(
        "application alias {alias} lists {identifier:?} under {platform}; an alias lists platform identifiers, not other aliases"
    )]
    AliasIdentifier {
        /// The alias listing it.
        alias: AppAliasName,
        /// The field it is listed under.
        platform: &'static str,
        /// The `app:` identifier.
        identifier: String,
    },
    /// A Windows identifier has an upper-case letter, which the lower-case
    /// identifier Windows reports can never equal.
    #[error(
        "application alias {alias} lists {identifier:?} under windows; Windows identifiers are lower-case, as OpenLogi reports them"
    )]
    UppercaseWindowsIdentifier {
        /// The alias listing it.
        alias: AppAliasName,
        /// The identifier with an upper-case letter.
        identifier: String,
    },
    /// One identifier is listed twice for a platform.
    #[error(
        "{platform} identifier {identifier:?} is listed by application aliases {first} and {second}; an application can have only one alias"
    )]
    DuplicateIdentifier {
        /// The field it is listed under.
        platform: &'static str,
        /// The repeated identifier.
        identifier: String,
        /// The alias that lists it first, in name order.
        first: AppAliasName,
        /// The alias that lists it again; the same as `first` when one alias
        /// repeats it.
        second: AppAliasName,
    },
}

impl TryFrom<BTreeMap<AppAliasName, AppAlias>> for AppAliases {
    type Error = AppAliasError;

    fn try_from(aliases: BTreeMap<AppAliasName, AppAlias>) -> Result<Self, Self::Error> {
        for platform in AppPlatform::ALL {
            let mut owners: BTreeMap<&str, &AppAliasName> = BTreeMap::new();
            for (name, alias) in &aliases {
                for identifier in alias.identifiers(platform) {
                    check_identifier(name, platform, identifier)?;
                    if let Some(first) = owners.insert(identifier, name) {
                        return Err(AppAliasError::DuplicateIdentifier {
                            platform: platform.field(),
                            identifier: identifier.clone(),
                            first: first.clone(),
                            second: name.clone(),
                        });
                    }
                }
            }
        }
        Ok(Self(aliases))
    }
}

fn check_identifier(
    alias: &AppAliasName,
    platform: AppPlatform,
    identifier: &str,
) -> Result<(), AppAliasError> {
    if identifier.is_empty() {
        return Err(AppAliasError::EmptyIdentifier {
            alias: alias.clone(),
            platform: platform.field(),
        });
    }
    if identifier.starts_with(PROFILE_KEY_PREFIX) {
        return Err(AppAliasError::AliasIdentifier {
            alias: alias.clone(),
            platform: platform.field(),
            identifier: identifier.to_string(),
        });
    }
    if platform == AppPlatform::Windows && identifier.to_lowercase() != identifier {
        return Err(AppAliasError::UppercaseWindowsIdentifier {
            alias: alias.clone(),
            identifier: identifier.to_string(),
        });
    }
    Ok(())
}

impl Serialize for AppAliases {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AppAliases {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let aliases = BTreeMap::<AppAliasName, AppAlias>::deserialize(deserializer)?;
        Self::try_from(aliases).map_err(de::Error::custom)
    }
}

impl AppAliases {
    /// Whether the table defines no alias, so `config.toml` can omit it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every alias, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&AppAliasName, &AppAlias)> {
        self.0.iter()
    }

    /// The alias a per-app profile key names: `Some` for an `app:<alias>` key
    /// this table defines, `None` for a platform identifier key — and for an
    /// `app:` key naming no alias here, which a loaded config never holds.
    #[must_use]
    pub fn for_profile_key(&self, key: &str) -> Option<(&AppAliasName, &AppAlias)> {
        let name = AppAliasName::try_new(key.strip_prefix(PROFILE_KEY_PREFIX)?.to_string()).ok()?;
        self.0.get_key_value(&name)
    }

    /// Whether `key` is an `app:` key naming an alias this table does not
    /// define. A platform identifier key is never one.
    #[must_use]
    pub fn is_undefined_reference(&self, key: &str) -> bool {
        key.starts_with(PROFILE_KEY_PREFIX) && self.for_profile_key(key).is_none()
    }

    /// The alias that lists `identifier` for `platform`. Validation makes it
    /// unique.
    pub(crate) fn claiming(
        &self,
        platform: AppPlatform,
        identifier: &str,
    ) -> Option<&AppAliasName> {
        self.0.iter().find_map(|(name, alias)| {
            alias
                .identifiers(platform)
                .iter()
                .any(|listed| listed == identifier)
                .then_some(name)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(source: &str) -> Result<AppAliases, toml::de::Error> {
        #[derive(Deserialize)]
        struct Document {
            apps: AppAliases,
        }
        toml::from_str::<Document>(source).map(|document| document.apps)
    }

    fn name(name: &str) -> AppAliasName {
        AppAliasName::try_new(name.to_string()).expect("valid alias name")
    }

    #[test]
    fn a_platform_field_takes_one_identifier_or_several() {
        let table = aliases(
            r#"
            [apps.chrome]
            name = "Google Chrome"
            macos = "com.google.Chrome"
            windows = ["exe:chrome.exe", 'c:\tools\chrome\chrome.exe']
            "#,
        )
        .expect("documented alias must parse");
        let (alias_name, chrome) = table
            .for_profile_key("app:chrome")
            .expect("app:chrome names the alias");
        assert_eq!(alias_name, &name("chrome"));
        assert_eq!(chrome.display_name(), Some("Google Chrome"));
        assert_eq!(
            chrome.identifiers(AppPlatform::MacOs),
            ["com.google.Chrome"]
        );
        assert_eq!(
            chrome.identifiers(AppPlatform::Windows),
            ["exe:chrome.exe", r"c:\tools\chrome\chrome.exe"]
        );
        assert!(chrome.identifiers(AppPlatform::Linux).is_empty());
    }

    #[test]
    fn one_identifier_saves_as_a_string_and_several_as_an_array() {
        let table = aliases(
            r#"
            [apps.code]
            macos = ["com.microsoft.VSCode"]
            linux = ["code", "Code"]
            "#,
        )
        .expect("alias must parse");
        let saved = toml::to_string(&table).expect("alias must serialize");
        assert_eq!(
            saved,
            "[code]\nmacos = \"com.microsoft.VSCode\"\nlinux = [\"code\", \"Code\"]\n"
        );
        let reloaded = aliases(&format!(
            "[apps]\n{}",
            saved.replace("[code]", "[apps.code]")
        ))
        .expect("saved alias must parse");
        assert_eq!(reloaded, table);
    }

    #[test]
    fn a_name_without_text_is_no_display_name() {
        let table = aliases("[apps.editor]\nname = \"  \"\nlinux = \"editor\"\n")
            .expect("alias must parse");
        let (_, editor) = table.for_profile_key("app:editor").expect("alias");
        assert_eq!(editor.display_name(), None);
    }

    #[test]
    fn alias_names_are_toml_bare_keys() {
        for valid in ["chrome", "VS_Code", "photoshop-2026", "7zip"] {
            assert!(
                AppAliasName::try_new(valid.to_string()).is_ok(),
                "{valid} must be a valid alias name"
            );
        }
        assert_eq!(
            AppAliasName::try_new(String::new()),
            Err(AppAliasNameError::Empty)
        );
        for invalid in ["google chrome", "a.b", "app:chrome", "café"] {
            assert_eq!(
                AppAliasName::try_new(invalid.to_string()),
                Err(AppAliasNameError::InvalidCharacter(invalid.to_string())),
                "{invalid} must be rejected"
            );
        }
        let error = aliases("[apps.\"google chrome\"]\nmacos = \"com.google.Chrome\"\n")
            .expect_err("a quoted name with a space must fail to load");
        assert!(
            error.to_string().contains("google chrome"),
            "the error names the alias: {error}"
        );
    }

    #[test]
    fn invalid_identifiers_fail_to_load() {
        for (source, expected) in [
            (
                "[apps.a]\nmacos = \"\"\n",
                "lists an empty macos identifier",
            ),
            ("[apps.a]\nlinux = \"app:b\"\n", "not other aliases"),
            (
                "[apps.a]\nwindows = \"exe:Chrome.exe\"\n",
                "Windows identifiers are lower-case",
            ),
            (
                "[apps.a]\nmacos = [\"com.example.App\", \"com.example.App\"]\n",
                "listed by application aliases a and a",
            ),
            (
                "[apps.a]\nlinux = \"firefox\"\n[apps.b]\nlinux = \"firefox\"\n",
                "listed by application aliases a and b",
            ),
            ("[apps.a]\nios = \"com.example.App\"\n", "unknown field"),
            (
                "[apps.a]\nmacos = 1\n",
                "an application identifier or an array",
            ),
        ] {
            let error = aliases(source).expect_err(source);
            assert!(
                error.to_string().contains(expected),
                "{source:?} must fail with {expected:?}, got: {error}"
            );
        }
    }

    #[test]
    fn upper_case_is_an_ordinary_identifier_outside_windows() {
        // Bundle identifiers and Linux classes keep their case: `Code` is a
        // real `WM_CLASS`.
        let table = aliases("[apps.code]\nmacos = \"com.Microsoft.VSCode\"\nlinux = \"Code\"\n")
            .expect("mixed-case identifiers outside Windows must load");
        assert_eq!(
            table.claiming(AppPlatform::Linux, "Code"),
            Some(&name("code"))
        );
    }

    #[test]
    fn one_identifier_may_appear_once_per_platform() {
        // Listing the same string under two platforms is no conflict: each
        // platform only ever reports its own namespace.
        let table = aliases("[apps.a]\nmacos = \"editor\"\n[apps.b]\nlinux = \"editor\"\n")
            .expect("the same string on two platforms must load");
        assert_eq!(
            table.claiming(AppPlatform::MacOs, "editor"),
            Some(&name("a"))
        );
        assert_eq!(
            table.claiming(AppPlatform::Linux, "editor"),
            Some(&name("b"))
        );
        assert_eq!(table.claiming(AppPlatform::Windows, "editor"), None);
    }

    #[test]
    fn profile_keys_name_aliases_only_with_the_prefix() {
        let table = aliases("[apps.chrome]\nmacos = \"com.google.Chrome\"\n").expect("alias");
        assert_eq!(name("chrome").profile_key(), "app:chrome");
        assert!(table.for_profile_key("chrome").is_none());
        assert!(table.for_profile_key("com.google.Chrome").is_none());
        assert!(!table.is_undefined_reference("com.google.Chrome"));
        assert!(!table.is_undefined_reference("app:chrome"));
        for undefined in ["app:chrom", "app:", "app:bad name"] {
            assert!(table.for_profile_key(undefined).is_none(), "{undefined}");
            assert!(table.is_undefined_reference(undefined), "{undefined}");
        }
    }
}

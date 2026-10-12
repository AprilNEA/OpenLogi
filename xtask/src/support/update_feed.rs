//! Where the stable channel's update feeds live on the update host.
//!
//! `macos package` stamps the Sparkle feed URL into the bundle's `Info.plist`,
//! and `release appcast` writes the feed that URL names; both derive it here,
//! so the feed an installed app reads and the file a release uploads cannot
//! drift apart. `release latest-json` writes the built-in engine's manifest
//! into the same channel.

/// The channel every release publishes to.
pub(crate) const CHANNEL: &str = "stable";

/// The variable the update host is read from, by every command that builds
/// or publishes a feed on it.
pub(crate) const BASE_URL_ENV: &str = "OPENLOGI_UPDATE_BASE_URL";

/// The variable Sparkle's base64 EdDSA public key (`SUPublicEDKey`) is read
/// from — what Sparkle's `generate_keys` prints. Not a secret. Packaging
/// embeds it; the appcast step checks its signing key against it.
pub(crate) const SPARKLE_PUBLIC_KEY_ENV: &str = "OPENLOGI_SPARKLE_PUBLIC_ED_KEY";

/// `LSMinimumSystemVersion` of the shipped bundle: `macos package` stamps it
/// (`support::info_plist`), `actool` targets it, and every feed advertises it.
pub(crate) const MACOS_MINIMUM_OS_VERSION: &str = "13.0";

/// A macOS release architecture, spelled as the DMG names and feeds spell it.
///
/// Each architecture ships its own DMG, so each reads its own Sparkle feed: a
/// single feed could only offer one of the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::VariantArray)]
pub(crate) enum MacArch {
    Arm64,
    X86_64,
}

impl MacArch {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::X86_64 => "x86_64",
        }
    }

    /// The architecture this xtask runs on, which is what a `macos package`
    /// without `--target` builds for.
    pub(crate) fn host() -> Option<Self> {
        match std::env::consts::ARCH {
            "aarch64" => Some(Self::Arm64),
            "x86_64" => Some(Self::X86_64),
            _ => None,
        }
    }

    /// The architecture of a release DMG, from its `…-macos-<arch>.dmg` name.
    pub(crate) fn of_dmg(file_name: &str) -> Option<Self> {
        let (_, arch) = file_name.strip_suffix(".dmg")?.rsplit_once("-macos-")?;
        match arch {
            "arm64" => Some(Self::Arm64),
            "x86_64" => Some(Self::X86_64),
            _ => None,
        }
    }

    /// File name of this architecture's Sparkle feed on the channel.
    pub(crate) fn appcast_file_name(self) -> String {
        format!("appcast-{}.xml", self.as_str())
    }
}

/// URL of the Sparkle feed `arch`'s bundles read from the update host at
/// `base_url`.
pub(crate) fn appcast_url(base_url: &str, arch: MacArch) -> String {
    format!(
        "{}/channels/{CHANNEL}/{}",
        base_url.trim_end_matches('/'),
        arch.appcast_file_name()
    )
}

#[cfg(test)]
mod tests;

//! Where a release and its update feeds live on the update host.
//!
//! The paths and URLs are gpui-updater-release's [`FeedLayout`], set up here
//! with OpenLogi's names. `macos package` stamps the Sparkle feed URL into
//! the bundle's `Info.plist`, and `release channels` writes the feed that URL
//! names; both derive it here, so the feed an installed app reads and the
//! file a release uploads cannot drift apart. A channel's directory is
//! [`UpdateChannel::dir`], which the app reads its manifest from as well.
//!
//! Each release also keeps its own feed record next to its artifacts:
//! `release latest-json` and `release appcast` write it, and
//! `release channels` composes the channel files from the records. A record
//! is laid out like a channel directory ([`record_layout`]).

use gpui_updater_release::FeedLayout;
use openlogi_core::update_feed::{MANIFEST_FILE_NAME, UpdateChannel};

use crate::support::release_tag::ReleaseTag;

/// The channel whose directory holds the Sparkle feeds. Every bundle reads
/// them, whatever channel it follows: Sparkle filters a beta item by its
/// `sparkle:channel`, not by feed URL.
pub(crate) const APPCAST_CHANNEL: UpdateChannel = UpdateChannel::Stable;

/// The directory, relative to the root of the update host, that holds one
/// directory per release.
pub(crate) const RELEASES_DIR: &str = "releases";

/// The name of a release's feed record directory, inside the release's
/// directory.
pub(crate) const RECORD_DIR_NAME: &str = "feed";

/// File name of an architecture's Sparkle feed, in a channel directory and
/// in a record alike; `{arch}` is [`MacArch::as_str`].
const APPCAST_NAME: &str = "appcast-{arch}.xml";

/// The base URL of a layout used only for object keys (its `*_path`
/// methods), which do not depend on it: the paths `release channels` reads
/// from a mirror, and the ones the release commands write under the stage.
pub(crate) const KEYS_ONLY: &str = "";

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
}

/// The update host at `base_url` as `channel` serves it: the channel's
/// manifest and Sparkle feeds, and every release's files.
pub(crate) fn channel_layout(base_url: &str, channel: UpdateChannel) -> FeedLayout {
    FeedLayout::new(base_url, channel.as_str())
        .channel_dir(channel.dir())
        .manifest_name(MANIFEST_FILE_NAME)
        .appcast_name(APPCAST_NAME)
        .release_dir(format!("{RELEASES_DIR}/{{tag}}"))
}

/// `tag`'s feed record on the update host at `base_url`: its single-release
/// manifest and one single-item appcast per architecture, named as a channel
/// directory names them, in [`record_dir`].
pub(crate) fn record_layout(base_url: &str, tag: &ReleaseTag) -> FeedLayout {
    channel_layout(base_url, tag.channel()).channel_dir(record_dir(tag))
}

/// URL of the Sparkle feed `arch`'s bundles read from the update host at
/// `base_url`.
pub(crate) fn appcast_url(base_url: &str, arch: MacArch) -> String {
    channel_layout(base_url, APPCAST_CHANNEL).appcast_url(arch.as_str())
}

/// The directory of `tag`'s artifacts, relative to the root of the update
/// host. Its files never change once uploaded.
pub(crate) fn release_dir(tag: &ReleaseTag) -> String {
    channel_layout(KEYS_ONLY, tag.channel()).release_path(&tag.to_string())
}

/// The directory of `tag`'s feed record ([`record_layout`]), relative to the
/// root of the update host.
pub(crate) fn record_dir(tag: &ReleaseTag) -> String {
    format!("{}/{RECORD_DIR_NAME}", release_dir(tag))
}

/// URL of `tag`'s GitHub Release page.
pub(crate) fn release_page_url(tag: &ReleaseTag) -> String {
    format!("https://github.com/AprilNEA/OpenLogi/releases/tag/{tag}")
}

#[cfg(test)]
mod tests;

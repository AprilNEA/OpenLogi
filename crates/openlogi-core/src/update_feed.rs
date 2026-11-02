//! Where OpenLogi's update feeds live on the update host.
//!
//! The host serves one directory per [`UpdateChannel`]. The app reads the
//! built-in engine's manifest from it, packaging stamps the Sparkle feed URL
//! from it, and xtask writes both feeds for it. All of them derive the
//! location here, so the URL an installed app reads and the feed xtask writes
//! for that URL cannot drift apart. xtask stages the channel files in the
//! same layout, and the release workflow copies the staged tree to the host
//! as it is.

use serde::{Deserialize, Serialize};

/// File name of the built-in update engine's manifest in a channel's
/// directory.
pub const MANIFEST_FILE_NAME: &str = "latest.json";

/// A release channel: the set of releases an installation is offered.
///
/// Settings persist the channel a user picks by its name, for example
/// `update_channel = "beta"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Final releases, tagged `vX.Y.Z`.
    Stable,
    /// Pre-releases, tagged `vX.Y.Z-beta.N`. Its manifest offers the newest
    /// beta, or the newest stable release when that one is newer.
    Beta,
}

impl UpdateChannel {
    /// Every channel, in the order Settings offers them.
    pub const ALL: [Self; 2] = [Self::Stable, Self::Beta];

    /// The channel an installation follows: `chosen`, the one the user picked,
    /// or else the channel of `running_version`. Without a pick, a pre-release
    /// such as `0.8.17-beta.12` follows beta and a final release follows
    /// stable, so an unpicked beta that updates to a final release follows
    /// stable from then on. A picked channel stays until the user changes it.
    /// Every reader of the channel decides it here.
    #[must_use]
    pub fn followed(chosen: Option<Self>, running_version: &str) -> Self {
        chosen.unwrap_or_else(|| Self::of_version(running_version))
    }

    /// The channel `version` comes from: a semantic-version pre-release is a
    /// beta. The pre-release starts at the first `-` before any `+`, because
    /// build metadata may contain `-` too.
    fn of_version(version: &str) -> Self {
        let without_build = version.split_once('+').map_or(version, |(head, _)| head);
        if without_build.contains('-') {
            Self::Beta
        } else {
            Self::Stable
        }
    }

    /// The channel's name, as the feed layout and the feeds themselves spell
    /// it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }

    /// This channel's directory, relative to the root of the update host.
    /// Every feed of the channel is a file directly inside it.
    #[must_use]
    pub fn dir(self) -> String {
        format!("channels/{}", self.as_str())
    }

    /// URL of this channel's directory ([`Self::dir`]) on the update host at
    /// `base_url`. Trailing slashes on `base_url` are dropped, so
    /// `https://host/` and `https://host` name the same directory.
    #[must_use]
    pub fn dir_url(self, base_url: &str) -> String {
        format!("{}/{}", base_url.trim_end_matches('/'), self.dir())
    }

    /// URL of the built-in update engine's manifest for this channel on the
    /// update host at `base_url`.
    #[must_use]
    pub fn manifest_url(self, base_url: &str) -> String {
        format!("{}/{MANIFEST_FILE_NAME}", self.dir_url(base_url))
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateChannel;

    #[test]
    fn each_channel_reads_its_own_directory_on_the_host() {
        assert_eq!(UpdateChannel::Beta.dir(), "channels/beta");
        assert_eq!(
            UpdateChannel::Stable.dir_url("https://updates.openlogi.org"),
            "https://updates.openlogi.org/channels/stable"
        );
        assert_eq!(
            UpdateChannel::Stable.manifest_url("https://mirror.example.org/"),
            "https://mirror.example.org/channels/stable/latest.json"
        );
        assert_eq!(
            UpdateChannel::Beta.manifest_url("https://updates.openlogi.org"),
            "https://updates.openlogi.org/channels/beta/latest.json"
        );
    }

    #[test]
    fn a_chosen_channel_wins_over_the_running_version() {
        for version in ["0.8.16", "0.8.17-beta.12"] {
            for channel in UpdateChannel::ALL {
                assert_eq!(UpdateChannel::followed(Some(channel), version), channel);
            }
        }
    }

    #[test]
    fn without_a_choice_a_pre_release_follows_beta() {
        for (version, channel) in [
            ("0.8.16", UpdateChannel::Stable),
            ("0.8.16+build-7", UpdateChannel::Stable),
            ("0.8.17-beta.12", UpdateChannel::Beta),
            ("0.8.17-beta.12+build-7", UpdateChannel::Beta),
        ] {
            assert_eq!(UpdateChannel::followed(None, version), channel, "{version}");
        }
    }

    #[test]
    fn a_base_url_with_a_path_keeps_it() {
        assert_eq!(
            UpdateChannel::Stable.manifest_url("https://cdn.example.org/openlogi//"),
            "https://cdn.example.org/openlogi/channels/stable/latest.json"
        );
    }
}

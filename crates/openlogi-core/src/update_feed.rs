//! Where OpenLogi's update feeds live on the update host.
//!
//! The host serves one directory per [`UpdateChannel`]. The app reads the
//! built-in engine's manifest from it, packaging stamps the Sparkle feed URL
//! from it, and xtask writes both feeds for it. All of them derive the
//! location here, so the URL an installed app reads and the feed xtask writes
//! for that URL cannot drift apart. The release workflow's upload
//! destinations (`release.yml`) still spell the layout themselves.

/// A release channel: the set of releases an installation is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    /// Final releases, tagged `vX.Y.Z`.
    Stable,
    /// Pre-releases, tagged `vX.Y.Z-beta.N`. Nothing publishes to it yet.
    Beta,
}

impl UpdateChannel {
    /// The channel's name, as the feed layout and the feeds themselves spell
    /// it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }

    /// URL of this channel's directory on the update host at `base_url`.
    /// Every feed of the channel is a file directly inside it. Trailing
    /// slashes on `base_url` are dropped, so `https://host/` and
    /// `https://host` name the same directory.
    #[must_use]
    pub fn dir_url(self, base_url: &str) -> String {
        format!(
            "{}/channels/{}",
            base_url.trim_end_matches('/'),
            self.as_str()
        )
    }

    /// URL of the built-in update engine's manifest for this channel on the
    /// update host at `base_url`.
    #[must_use]
    pub fn manifest_url(self, base_url: &str) -> String {
        format!("{}/latest.json", self.dir_url(base_url))
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateChannel;

    #[test]
    fn each_channel_reads_its_own_directory_on_the_host() {
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
    fn a_base_url_with_a_path_keeps_it() {
        assert_eq!(
            UpdateChannel::Stable.manifest_url("https://cdn.example.org/openlogi//"),
            "https://cdn.example.org/openlogi/channels/stable/latest.json"
        );
    }
}

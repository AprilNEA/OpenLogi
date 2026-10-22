//! Where OpenLogi's update feeds live on the update host.
//!
//! The host serves one directory per [`UpdateChannel`]. The app reads the
//! built-in engine's manifest from it, packaging stamps the Sparkle feed URL
//! from it, and the release workflow publishes both into it. Every one of them
//! derives the location here, so the file an installed app reads and the file
//! a release uploads cannot drift apart.

/// A release channel: the set of releases an installation is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    /// Every tagged release. Today the only channel releases publish to.
    Stable,
}

impl UpdateChannel {
    /// The channel's name, as the feed layout and the feeds themselves spell
    /// it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
        }
    }

    /// URL of this channel's directory on the update host at `base_url`.
    /// Every feed of the channel is a file directly inside it.
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
    }

    #[test]
    fn a_base_url_with_a_path_keeps_it() {
        assert_eq!(
            UpdateChannel::Stable.manifest_url("https://cdn.example.org/openlogi//"),
            "https://cdn.example.org/openlogi/channels/stable/latest.json"
        );
    }
}

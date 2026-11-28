//! GitHub Releases version check for Linux package installs.
//!
//! The engine-based manifest check in [`crate::platform::updater`] never has a
//! Linux asset to find (`.deb`/`.rpm`/`.pkg.tar.zst` installs update through
//! the OS package manager, not the in-app engine — see
//! `xtask/src/commands/release/latest_json.rs`).
//! This checks the project's GitHub Releases directly instead, purely to learn
//! whether a newer version exists; installing it is [`super::package_install`].

use gpui_updater::Version;
use openlogi_core::update_feed::UpdateChannel;
use serde::Deserialize;

/// The project's GitHub Releases API endpoint.
const RELEASES_API_URL: &str = "https://api.github.com/repos/AprilNEA/OpenLogi/releases";

/// How many of the most recent releases the Beta channel looks at. GitHub
/// lists them newest first, so the newest beta and the newest stable release
/// are both on this first page.
const BETA_RELEASES_PAGE: u8 = 30;

/// One asset attached to a GitHub release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    assets: Vec<ReleaseAsset>,
}

/// The newest release the followed channel offers, newer than the running
/// build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub version: Version,
    pub assets: Vec<ReleaseAsset>,
}

/// Outcome of comparing the channel's newest GitHub release against the
/// running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxUpdateCheck {
    UpToDate,
    Available(LatestRelease),
}

/// Fetch the newest release `channel` offers from GitHub and compare it
/// against `current`. Stable reads `/releases/latest`, which skips
/// prereleases; Beta takes the highest version among the recent releases,
/// prereleases included, so it also gets every newer stable release.
/// Blocking — run on a background executor, never the UI thread.
pub fn check_latest_release(
    agent: &ureq::Agent,
    channel: UpdateChannel,
    current: &Version,
) -> Result<LinuxUpdateCheck, String> {
    let url = match channel {
        UpdateChannel::Stable => format!("{RELEASES_API_URL}/latest"),
        UpdateChannel::Beta => format!("{RELEASES_API_URL}?per_page={BETA_RELEASES_PAGE}"),
    };
    let body = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header(
            "User-Agent",
            concat!("openlogi-desktop/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| format!("GitHub releases request failed: {error}"))?
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("failed to read GitHub releases response: {error}"))?;
    let release = match channel {
        UpdateChannel::Stable => parse_latest(&body)?,
        UpdateChannel::Beta => parse_newest(&body)?,
    };
    Ok(compare(release, current))
}

/// The release a `/releases/latest` response describes.
fn parse_latest(body: &str) -> Result<LatestRelease, String> {
    let release: ReleaseResponse = serde_json::from_str(body)
        .map_err(|error| format!("failed to parse GitHub release metadata: {error}"))?;
    versioned(release)
}

/// The highest-versioned published release in a `/releases` list response.
/// Drafts and tags that are not semver are skipped.
fn parse_newest(body: &str) -> Result<LatestRelease, String> {
    let releases: Vec<ReleaseResponse> = serde_json::from_str(body)
        .map_err(|error| format!("failed to parse GitHub release metadata: {error}"))?;
    releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| versioned(release).ok())
        .max_by(|a, b| a.version.cmp(&b.version))
        .ok_or_else(|| "GitHub lists no release with a semver tag".to_string())
}

fn versioned(release: ReleaseResponse) -> Result<LatestRelease, String> {
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(tag)
        .map_err(|error| format!("GitHub release tag {tag:?} is not valid semver: {error}"))?;
    Ok(LatestRelease {
        version,
        assets: release.assets,
    })
}

fn compare(release: LatestRelease, current: &Version) -> LinuxUpdateCheck {
    if release.version > *current {
        LinuxUpdateCheck::Available(release)
    } else {
        LinuxUpdateCheck::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modeled on the real GitHub API response shape (trimmed to the fields
    /// this module reads).
    const FIXTURE: &str = r#"{
        "tag_name": "v0.8.15",
        "assets": [
            {
                "name": "openlogi-v0.8.15-linux-amd64.deb",
                "browser_download_url": "https://example.test/openlogi-v0.8.15-linux-amd64.deb"
            },
            {
                "name": "openlogi-v0.8.15-linux-amd64.deb.minisig",
                "browser_download_url": "https://example.test/openlogi-v0.8.15-linux-amd64.deb.minisig"
            },
            {
                "name": "SHA256SUMS",
                "browser_download_url": "https://example.test/SHA256SUMS"
            }
        ]
    }"#;

    fn parse_and_compare(body: &str, current: &Version) -> Result<LinuxUpdateCheck, String> {
        parse_latest(body).map(|release| compare(release, current))
    }

    #[test]
    fn newer_tag_reports_available_with_its_assets() {
        let current = Version::parse("0.8.14").unwrap();
        let LinuxUpdateCheck::Available(release) = parse_and_compare(FIXTURE, &current).unwrap()
        else {
            panic!("expected an available update");
        };
        assert_eq!(release.version, Version::parse("0.8.15").unwrap());
        assert_eq!(release.assets.len(), 3);
        assert_eq!(release.assets[0].name, "openlogi-v0.8.15-linux-amd64.deb");
    }

    #[test]
    fn same_or_older_tag_reports_up_to_date() {
        for current in ["0.8.15", "0.9.0"] {
            let current = Version::parse(current).unwrap();
            assert_eq!(
                parse_and_compare(FIXTURE, &current).unwrap(),
                LinuxUpdateCheck::UpToDate
            );
        }
    }

    #[test]
    fn malformed_json_is_a_parse_error_not_a_panic() {
        let current = Version::parse("0.8.14").unwrap();
        parse_and_compare("not json", &current).unwrap_err();
    }

    #[test]
    fn non_semver_tag_is_a_parse_error_not_a_panic() {
        let current = Version::parse("0.8.14").unwrap();
        let body = r#"{"tag_name": "nightly", "assets": []}"#;
        parse_and_compare(body, &current).unwrap_err();
    }

    /// A `/releases` list, newest first as GitHub returns it: a draft, a
    /// beta, the stable release it follows, a non-semver tag, an older beta.
    const LIST_FIXTURE: &str = r#"[
        {"tag_name": "v0.9.0-beta.1", "draft": true, "assets": []},
        {"tag_name": "v0.8.19-beta.2", "draft": false, "assets": [
            {"name": "SHA256SUMS", "browser_download_url": "https://example.test/SHA256SUMS"}
        ]},
        {"tag_name": "v0.8.18", "draft": false, "assets": []},
        {"tag_name": "nightly", "draft": false, "assets": []},
        {"tag_name": "v0.8.19-beta.1", "draft": false, "assets": []}
    ]"#;

    #[test]
    fn beta_picks_the_highest_published_version_prereleases_included() {
        let newest = parse_newest(LIST_FIXTURE).unwrap();
        assert_eq!(newest.version, Version::parse("0.8.19-beta.2").unwrap());
        assert_eq!(newest.assets.len(), 1);

        let current = Version::parse("0.8.18").unwrap();
        assert!(matches!(
            compare(newest.clone(), &current),
            LinuxUpdateCheck::Available(_)
        ));
        let current = Version::parse("0.8.19-beta.2").unwrap();
        assert_eq!(compare(newest, &current), LinuxUpdateCheck::UpToDate);
    }

    #[test]
    fn beta_takes_a_stable_release_numbered_past_every_beta() {
        let body = r#"[
            {"tag_name": "v0.8.19", "assets": []},
            {"tag_name": "v0.8.19-beta.2", "assets": []}
        ]"#;
        assert_eq!(
            parse_newest(body).unwrap().version,
            Version::parse("0.8.19").unwrap()
        );
    }

    #[test]
    fn beta_without_a_semver_release_is_an_error_not_a_panic() {
        parse_newest("[]").unwrap_err();
        parse_newest(r#"[{"tag_name": "nightly", "assets": []}]"#).unwrap_err();
        parse_newest("not json").unwrap_err();
    }
}

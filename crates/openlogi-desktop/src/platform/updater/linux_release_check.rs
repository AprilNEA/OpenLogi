//! GitHub Releases version check for Linux package installs.
//!
//! The engine-based manifest check in [`super`] never has a Linux asset to
//! find (`.deb`/`.rpm`/`.pkg.tar.zst` installs update through the OS package
//! manager, not the in-app engine — see `xtask/src/commands/release/latest_json.rs`).
//! This checks the project's GitHub Releases directly instead, purely to learn
//! whether a newer version exists; installing it is [`super::linux_package_install`].

use gpui_updater::Version;
use serde::Deserialize;

/// The project's GitHub Releases API endpoint for the latest published release.
const RELEASES_API_URL: &str = "https://api.github.com/repos/AprilNEA/OpenLogi/releases/latest";

/// One asset attached to a GitHub release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

/// The latest published release, newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub version: Version,
    pub assets: Vec<ReleaseAsset>,
}

/// Outcome of comparing the latest GitHub release against the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxUpdateCheck {
    UpToDate,
    Available(LatestRelease),
}

/// Fetch the latest release from GitHub and compare it against `current`.
/// Blocking — run on a background executor, never the UI thread.
pub fn check_latest_release(current: &Version) -> Result<LinuxUpdateCheck, String> {
    let body = ureq::get(RELEASES_API_URL)
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
    parse_and_compare(&body, current)
}

/// Pure parse + compare, split out from [`check_latest_release`] so it can be
/// unit-tested against a fixture JSON string with no network call.
fn parse_and_compare(body: &str, current: &Version) -> Result<LinuxUpdateCheck, String> {
    let release: ReleaseResponse = serde_json::from_str(body)
        .map_err(|error| format!("failed to parse GitHub release metadata: {error}"))?;
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(tag)
        .map_err(|error| format!("GitHub release tag {tag:?} is not valid semver: {error}"))?;
    if version > *current {
        Ok(LinuxUpdateCheck::Available(LatestRelease {
            version,
            assets: release.assets,
        }))
    } else {
        Ok(LinuxUpdateCheck::UpToDate)
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
}

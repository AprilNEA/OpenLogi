//! What a release tag says about the release it names, and the version
//! installers compare.
//!
//! release-plz tags a stable release `vX.Y.Z`. A beta is tagged
//! `vX.Y.Z-beta.N`: `X.Y.Z` is the stable release it leads to, and `N` counts
//! the commits since the stable release before it, so a later beta has a
//! larger `N`. Every command that reads a release tag parses it here.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context as _, Result, bail, ensure};
use openlogi_core::update_feed::UpdateChannel;
use semver::Version;

/// The pre-release identifier of a beta tag, before its number.
const BETA_PREFIX: &str = "beta.";

/// The variable a release build's [`BuildVersion`] is passed to packaging in,
/// as `release version` prints it.
pub(crate) const BUILD_VERSION_ENV: &str = "OPENLOGI_BUILD_VERSION";

/// A parsed release tag: `vX.Y.Z` or `vX.Y.Z-beta.N`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseTag {
    version: Version,
}

impl ReleaseTag {
    /// Parse `tag`. Any shape other than `vX.Y.Z` and `vX.Y.Z-beta.N` is an
    /// error, including build metadata and other pre-release identifiers.
    pub(crate) fn parse(tag: &str) -> Result<Self> {
        let Some(version) = tag.strip_prefix('v') else {
            bail!("release tag {tag:?} does not start with `v`");
        };
        let version = Version::parse(version)
            .with_context(|| format!("release tag {tag:?} is not `v` and a semantic version"))?;
        ensure!(
            version.build.is_empty(),
            "release tag {tag:?} carries build metadata; release tags are `vX.Y.Z` or `vX.Y.Z-beta.N`"
        );
        let parsed = Self { version };
        ensure!(
            parsed.version.pre.is_empty() || parsed.beta_number().is_some(),
            "release tag {tag:?} is a pre-release other than `beta.N`"
        );
        Ok(parsed)
    }

    /// The version of a stable release tag; `None` for a beta tag or any
    /// other tag.
    pub(crate) fn stable_version(tag: &str) -> Option<Version> {
        Self::parse(tag)
            .ok()
            .filter(|parsed| parsed.channel() == UpdateChannel::Stable)
            .map(|parsed| parsed.version)
    }

    /// The channel the release publishes to.
    pub(crate) fn channel(&self) -> UpdateChannel {
        if self.version.pre.is_empty() {
            UpdateChannel::Stable
        } else {
            UpdateChannel::Beta
        }
    }

    /// The release version, `X.Y.Z` or `X.Y.Z-beta.N`. The workspace version
    /// of the tagged commit equals it.
    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    /// `N` of a beta tag; `None` for a stable tag.
    fn beta_number(&self) -> Option<u64> {
        let number = self.version.pre.as_str().strip_prefix(BETA_PREFIX)?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        number.parse().ok()
    }
}

impl fmt::Display for ReleaseTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.version)
    }
}

/// The numeric version installers compare: the bundle's `CFBundleVersion`,
/// which Sparkle orders releases by, and the MSI `ProductVersion`.
///
/// A stable release's build version is its version, `X.Y.Z`. A beta's is
/// `B.N`, where `B` is the stable release before it: `0.8.17-beta.12` after
/// `0.8.16` builds as `0.8.16.12`. That orders after `0.8.16` and before
/// `0.8.17`, so a beta replaces the stable release it follows, and the next
/// stable release replaces the beta.
///
/// Windows Installer ignores the fourth field when it compares versions, so
/// every beta after `B` looks like `B` to it. `OpenLogi.wxs` sets
/// `AllowSameVersionUpgrades` for that reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildVersion {
    base: [u64; 3],
    beta: Option<u64>,
}

/// Windows Installer's limits on the fields of a `ProductVersion`: major and
/// minor up to 255, build and the fourth field up to 65535.
const MSI_FIELD_LIMITS: [u64; 4] = [255, 255, 65_535, 65_535];

impl BuildVersion {
    /// The build version of `tag`. For a beta, `merged_tags` lists the tags
    /// reachable from it; the newest stable release among them is `B`.
    pub(crate) fn of(tag: &ReleaseTag, merged_tags: &[String]) -> Result<Self> {
        let build = match tag.beta_number() {
            None => Self {
                base: numeric_fields(tag.version()),
                beta: None,
            },
            Some(number) => {
                let base = merged_tags
                    .iter()
                    .filter_map(|merged| ReleaseTag::stable_version(merged))
                    .max()
                    .with_context(|| {
                        format!("no stable release tag is reachable from {tag}; a beta follows one")
                    })?;
                ensure!(
                    &base < tag.version(),
                    "{tag} does not lead past v{base}, the stable release before it"
                );
                Self {
                    base: numeric_fields(&base),
                    beta: Some(number),
                }
            }
        };
        build.check_msi_limits()?;
        Ok(build)
    }

    fn fields(&self) -> impl Iterator<Item = u64> + '_ {
        self.base.iter().copied().chain(self.beta)
    }

    fn check_msi_limits(&self) -> Result<()> {
        for (field, limit) in self.fields().zip(MSI_FIELD_LIMITS) {
            ensure!(
                field <= limit,
                "build version {self} does not fit an MSI ProductVersion: a field is above {limit}"
            );
        }
        Ok(())
    }
}

fn numeric_fields(version: &Version) -> [u64; 3] {
    [version.major, version.minor, version.patch]
}

impl fmt::Display for BuildVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fields = self.fields();
        if let Some(first) = fields.next() {
            write!(f, "{first}")?;
        }
        for field in fields {
            write!(f, ".{field}")?;
        }
        Ok(())
    }
}

impl FromStr for BuildVersion {
    type Err = anyhow::Error;

    /// Parse a build version as [`fmt::Display`] writes it: three or four
    /// numeric fields without leading zeros, within the MSI limits.
    fn from_str(text: &str) -> Result<Self> {
        let fields = text
            .split('.')
            .map(|field| {
                ensure!(
                    !field.is_empty()
                        && field.bytes().all(|byte| byte.is_ascii_digit())
                        && (field == "0" || !field.starts_with('0')),
                    "build version {text:?} has a field that is not a number"
                );
                field
                    .parse()
                    .with_context(|| format!("build version {text:?} has a field out of range"))
            })
            .collect::<Result<Vec<u64>>>()?;
        let build = match fields[..] {
            [major, minor, patch] => Self {
                base: [major, minor, patch],
                beta: None,
            },
            [major, minor, patch, beta] => Self {
                base: [major, minor, patch],
                beta: Some(beta),
            },
            _ => bail!("build version {text:?} does not have three or four fields"),
        };
        build.check_msi_limits()?;
        Ok(build)
    }
}

#[cfg(test)]
mod tests;

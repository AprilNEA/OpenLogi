//! What a release tag says about the release it names, and the version
//! installers compare.
//!
//! release-plz tags a stable release `vX.Y.Z`. The prerelease workflow tags
//! a beta `vX.Y.Z-beta.N` on `master`, without a version bump: `X.Y.Z` is the
//! stable release it leads to, and `N` counts the commits since the stable
//! release before it. Within one stable release's cycle, a later beta has a
//! larger `N`; the count starts again after the next stable release.
//!
//! Every command that reads a release tag parses it here, and
//! [`ReleaseTag::beta`] spells the beta tag `release next-beta` names.

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
            "release tag {tag:?} is a pre-release other than `beta.N` with N at least 1"
        );
        Ok(parsed)
    }

    /// The beta tag `v<release>-beta.<number>`: beta `number` of the stable
    /// release `release` will be. Fails unless `release` is `X.Y.Z` and
    /// `number` is at least 1, the shapes [`Self::parse`] accepts.
    pub(crate) fn beta(release: &Version, number: u64) -> Result<Self> {
        ensure!(
            release.pre.is_empty() && release.build.is_empty(),
            "a beta leads to a stable release, and {release} is not `X.Y.Z`"
        );
        Self::parse(&format!("v{release}-{BETA_PREFIX}{number}"))
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

    /// The release version the tag names, `X.Y.Z` or `X.Y.Z-beta.N`. The
    /// tagged commit's workspace version is
    /// [`Self::tagged_workspace_version`], which differs for a beta.
    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    /// For a beta, the stable release it follows: the newest stable release
    /// tag in `merged_tags`, the tags reachable from the beta. `None` for a
    /// stable tag.
    pub(crate) fn beta_base(&self, merged_tags: &[String]) -> Result<Option<BetaBase>> {
        if self.beta_number().is_none() {
            return Ok(None);
        }
        let (version, tag) = merged_tags
            .iter()
            .filter_map(|merged| Some((Self::stable_version(merged)?, merged)))
            .max()
            .with_context(|| {
                format!("no stable release tag is reachable from {self}; a beta follows one")
            })?;
        ensure!(
            version < self.version,
            "{self} does not lead past {tag}, the stable release before it"
        );
        Ok(Some(BetaBase {
            tag: tag.clone(),
            version,
        }))
    }

    /// The release tag this release's notes compare with: the newest release
    /// tag in `merged_tags`, the tags reachable from this one, that is older
    /// than this one. A stable release skips betas, so its notes cover every
    /// change since the stable release before it; a beta compares with the
    /// release just before it, beta or stable. Tags that are not release tags
    /// are skipped. `None` when no older release tag is reachable.
    pub(crate) fn notes_base<'a>(&self, merged_tags: &'a [String]) -> Option<&'a str> {
        merged_tags
            .iter()
            .filter_map(|merged| Some((Self::parse(merged).ok()?, merged.as_str())))
            .filter(|(merged, _)| merged.version < self.version)
            .filter(|(merged, _)| {
                self.channel() == UpdateChannel::Beta || merged.channel() == UpdateChannel::Stable
            })
            .max_by(|(left, _), (right, _)| left.version.cmp(&right.version))
            .map(|(_, tag)| tag)
    }

    /// The workspace version the tagged commit carries. A stable tag is on
    /// release-plz's version bump, so it is the tag's own version. A beta tags
    /// `master` between releases, so it is `base`'s, the stable release the
    /// beta follows (from [`Self::beta_base`]); the beta's own version is
    /// stamped for its build only.
    pub(crate) fn tagged_workspace_version<'a>(
        &'a self,
        base: Option<&'a BetaBase>,
    ) -> &'a Version {
        base.map_or(&self.version, |base| &base.version)
    }

    /// The version a build of this tag stamps into the workspace before it
    /// compiles, with `release stamp-version`; `None` when the tagged commit
    /// already carries the tag's own version. A beta's tagged commit carries
    /// the stable release it follows, so its build stamps the beta's version,
    /// and every `CARGO_PKG_VERSION` in it reports the beta.
    pub(crate) fn stamp_version<'a>(&'a self, base: Option<&'a BetaBase>) -> Option<&'a Version> {
        (self.tagged_workspace_version(base) != &self.version).then_some(&self.version)
    }

    /// `N` of a beta tag; `None` for a stable tag. `N` is at least 1: Sparkle
    /// pads a missing field with 0, so `B.0` would equal `B`.
    fn beta_number(&self) -> Option<u64> {
        let number = self.version.pre.as_str().strip_prefix(BETA_PREFIX)?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        number.parse().ok().filter(|number| *number >= 1)
    }
}

/// The stable release a beta follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BetaBase {
    /// Its tag, for example `v0.8.16`.
    pub(crate) tag: String,
    version: Version,
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
///
/// The order is Sparkle's: field by field, and a version without a fourth
/// field before the same version with one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BuildVersion {
    base: [u64; 3],
    beta: Option<u64>,
}

/// Windows Installer's limits on the fields of a `ProductVersion`: major and
/// minor up to 255, build and the fourth field up to 65535.
const MSI_FIELD_LIMITS: [u64; 4] = [255, 255, 65_535, 65_535];

impl BuildVersion {
    /// The build version of `tag`. A beta also needs `beta`: the stable
    /// release it follows, from [`ReleaseTag::beta_base`], and the number of
    /// commits from that release to the beta, which `N` must equal.
    pub(crate) fn of(tag: &ReleaseTag, beta: Option<(&BetaBase, u64)>) -> Result<Self> {
        let build = match (tag.beta_number(), beta) {
            (None, None) => Self {
                base: numeric_fields(tag.version()),
                beta: None,
            },
            (Some(number), Some((base, commits))) => {
                ensure!(
                    number == commits,
                    "{tag} has beta number {number}, but {commits} commits lead from {} to it; a beta's number is that count",
                    base.tag
                );
                Self {
                    base: numeric_fields(&base.version),
                    beta: Some(number),
                }
            }
            (None, Some((base, _))) => {
                bail!(
                    "{tag} is a stable release, so it does not follow {}",
                    base.tag
                )
            }
            (Some(_), None) => {
                bail!("{tag} is a beta; its build version needs the stable release before it")
            }
        };
        build.check_msi_limits()?;
        Ok(build)
    }

    /// Check that a release tagged `tag` can carry this build version: a
    /// stable release's is its own version, and a beta's ends in the beta's
    /// `N`, after a stable release older than the beta.
    pub(crate) fn ensure_fits(&self, tag: &ReleaseTag) -> Result<()> {
        let tagged = numeric_fields(tag.version());
        match tag.beta_number() {
            None => ensure!(
                *self
                    == Self {
                        base: tagged,
                        beta: None
                    },
                "{tag} is a stable release, so its build version is {}, not {self}",
                Self {
                    base: tagged,
                    beta: None
                }
            ),
            Some(number) => {
                ensure!(
                    self.beta == Some(number),
                    "{tag} is beta {number}, so its build version ends in .{number}, not {self}"
                );
                ensure!(
                    self.base < tagged,
                    "{tag} follows a stable release older than itself, so its build version starts below {}, not {self}",
                    tag.version()
                );
            }
        }
        Ok(())
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
    /// numeric fields without leading zeros, within the MSI limits, and a
    /// fourth field of at least 1.
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
            [major, minor, patch, beta] => {
                // Sparkle pads a missing field with 0, so `B.0` would equal `B`.
                ensure!(beta >= 1, "build version {text:?} has a fourth field of 0");
                Self {
                    base: [major, minor, patch],
                    beta: Some(beta),
                }
            }
            _ => bail!("build version {text:?} does not have three or four fields"),
        };
        build.check_msi_limits()?;
        Ok(build)
    }
}

#[cfg(test)]
mod tests;

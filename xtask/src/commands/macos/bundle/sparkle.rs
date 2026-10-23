//! Sparkle.framework in the release bundle: the Sparkle release it embeds,
//! the `Info.plist` keys that configure it, and the bundle version it is
//! checked against.
//!
//! The mechanics are gpui-updater-release's: `sparkle::distribution` fetches
//! and checks the pinned archive, `sparkle::bundle` embeds the framework the
//! way Sparkle's documentation says (`ditto`, XPC services removed, license
//! copied), writes and reads back the keys, and orders Sparkle's code for
//! `codesign`. What stays here is OpenLogi's: the pin, the privacy settings,
//! and the rule that a beta's `CFBundleVersion` is its build version.
//!
//! The desktop app updates through Sparkle when the bundle embeds it and
//! through its built-in engine otherwise (`platform::updater` in
//! `openlogi-desktop`), so a package built without a Sparkle public key is a
//! working bundle, not a broken one. The keys stamped here are what keep
//! Sparkle inside OpenLogi's privacy promise; the module docs there say how
//! the app drives it.

use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use gpui_updater_release::sparkle::{
    self, EmbedOptions, SparkleDistribution, SparkleRelease, SparkleSettings,
};

use crate::support::info_plist::{self, BundleVersions};
use crate::support::update_feed::{self, MacArch, SPARKLE_PUBLIC_KEY_ENV as PUBLIC_KEY_ENV};

/// Sparkle's code in the inside-out order `codesign` must seal it, each with
/// the hardened runtime and no `--deep`; empty when the app embeds no
/// Sparkle.
pub(super) use gpui_updater_release::sparkle::code_inside_out;

/// The Sparkle release the bundle embeds. Pinned here rather than taken from
/// gpui-updater-release, so a dependency bump never changes the framework a
/// release ships.
const VERSION: &str = "2.10.0";
/// SHA-256 of that release's `Sparkle-<VERSION>.tar.xz` from GitHub.
const ARCHIVE_SHA256: &str = "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c";

/// What the embedded Sparkle is told: the feed it reads and the key every
/// update it installs must be signed with.
#[derive(Debug)]
pub(crate) struct Feed {
    settings: SparkleSettings,
}

impl Feed {
    /// The feed for `arch`'s bundle on the update host at `base_url`.
    pub(crate) fn new(public_key: &str, base_url: &str, arch: MacArch) -> Result<Self> {
        // A pasted private key would also be 32 bytes, but the release step
        // checks the signing key against this one, so that mix-up cannot ship.
        let settings = SparkleSettings::new(update_feed::appcast_url(base_url, arch), public_key)
            .with_context(|| format!("{PUBLIC_KEY_ENV} is not a Sparkle EdDSA public key"))?
            // OpenLogi never polls: Sparkle's schedule stays off, without the
            // second-launch prompt asking to turn it on. The app runs its one
            // opt-in launch check itself.
            .enable_automatic_checks(false)
            // ...yet "Automatically download and install updates" still works
            // for the checks the app does run. Without this Sparkle ties the
            // option to its own schedule.
            .allows_automatic_updates(true)
            // No system profile rides along with a feed request. Off is
            // Sparkle's default; spelled out because the promise depends on it.
            .enable_system_profiling(false)
            // Check the EdDSA signature before unpacking the download, not
            // after. The DMG's Developer ID signature remains the fallback
            // Sparkle allows for rotating the key.
            .verify_update_before_extraction(true)
            // A beta's `CFBundleVersion` is its build version, not its release
            // version, so [`configure`] checks it against that instead.
            .allow_any_bundle_version();
        Ok(Self { settings })
    }

    /// `SUFeedURL`.
    fn url(&self) -> &str {
        self.settings.feed_url.as_deref().unwrap_or_default()
    }
}

/// Embed the pinned Sparkle.framework into `app` and point it at `feed`,
/// after checking that `app` carries `versions`. Runs before signing: the
/// bundle's signature seals both.
pub(super) fn embed(root: &Path, app: &Path, feed: &Feed, versions: &BundleVersions) -> Result<()> {
    println!("==> Sparkle {VERSION} ({})", feed.url());
    let release = fetch_release(root)?;
    sparkle::embed(app, &release, &EmbedOptions::default())
        .with_context(|| format!("could not embed Sparkle {VERSION} into {}", app.display()))?;
    configure(&app.join("Contents/Info.plist"), feed, versions)
}

/// Stamp `feed`'s keys into `info_plist` and read every one back, after
/// checking the bundle version Sparkle will compare.
fn configure(info_plist: &Path, feed: &Feed, versions: &BundleVersions) -> Result<()> {
    // Sparkle offers an update only when the appcast's `sparkle:version` — the
    // release's build version — is newer than `CFBundleVersion`. cargo-bundle's
    // build timestamp there would outrank every release, so no update would
    // ever be offered.
    let bundle_version = info_plist::read_plist_string(info_plist, "CFBundleVersion")?;
    ensure!(
        bundle_version.as_deref() == Some(versions.build.as_str()),
        "{} has CFBundleVersion {bundle_version:?}; Sparkle needs the build version {:?} there",
        info_plist.display(),
        versions.build
    );
    feed.settings
        .apply(info_plist)
        .with_context(|| format!("could not configure Sparkle in {}", info_plist.display()))
}

/// The pinned Sparkle release, unpacked under `target/sparkle`: downloaded
/// unless a copy with the pinned hash is already there, and unpacked afresh
/// every time, so nothing left in that directory can reach the bundle.
pub(crate) fn fetch_release(root: &Path) -> Result<SparkleDistribution> {
    SparkleRelease::new(VERSION, ARCHIVE_SHA256)
        .fetch(&root.join("target/sparkle"))
        .with_context(|| format!("could not fetch Sparkle {VERSION}"))
}

#[cfg(test)]
mod tests;

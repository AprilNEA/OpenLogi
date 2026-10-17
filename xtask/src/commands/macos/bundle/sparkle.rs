//! Sparkle.framework in the release bundle: fetching the pinned release,
//! embedding it, the `Info.plist` keys that configure it, and the order its
//! code is signed in.
//!
//! The desktop app updates through Sparkle when the bundle embeds it and
//! through its built-in engine otherwise (`platform::updater` in
//! `openlogi-desktop`), so a package built without a Sparkle public key is a
//! working bundle, not a broken one. The keys stamped here are what keep
//! Sparkle inside OpenLogi's privacy promise; the module docs there say how
//! the app drives it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use plist::Value;
use sha2_hasher::Sha2Hasher as _;
use xshell::{Shell, cmd};

use crate::support::info_plist;
use crate::support::update_feed::{self, MacArch, SPARKLE_PUBLIC_KEY_ENV as PUBLIC_KEY_ENV};

/// The Sparkle release the bundle embeds.
const VERSION: &str = "2.10.0";
/// SHA-256 of that release's `Sparkle-<VERSION>.tar.xz` from GitHub.
const ARCHIVE_SHA256: &str = "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c";

/// Sparkle 2's framework version directory.
const FRAMEWORK_VERSION: &str = "B";

/// What the embedded Sparkle is told: the feed it reads and the key every
/// update it installs must be signed with.
#[derive(Debug)]
pub(crate) struct Feed {
    url: String,
    public_key: String,
}

impl Feed {
    /// The feed for `arch`'s bundle on the update host at `base_url`.
    pub(crate) fn new(public_key: &str, base_url: &str, arch: MacArch) -> Result<Self> {
        let public_key = public_key.trim();
        let decoded = BASE64
            .decode(public_key)
            .with_context(|| format!("{PUBLIC_KEY_ENV} is not base64"))?;
        // A pasted private key would also be 32 bytes, but the release step
        // checks the signing key against this one, so that mix-up cannot ship.
        ensure!(
            decoded.len() == 32,
            "{PUBLIC_KEY_ENV} must be a 32-byte Ed25519 public key, not {} bytes",
            decoded.len()
        );
        Ok(Self {
            url: update_feed::appcast_url(base_url, arch),
            public_key: public_key.to_owned(),
        })
    }

    /// The `Info.plist` entries that configure Sparkle.
    fn info_plist_entries(&self) -> [(&'static str, Value); 6] {
        [
            ("SUFeedURL", Value::String(self.url.clone())),
            ("SUPublicEDKey", Value::String(self.public_key.clone())),
            // OpenLogi never polls: Sparkle's schedule stays off, without the
            // second-launch prompt asking to turn it on. The app runs its one
            // opt-in launch check itself.
            ("SUEnableAutomaticChecks", Value::Boolean(false)),
            // ...yet "Automatically download and install updates" still works
            // for the checks the app does run. Without this Sparkle ties the
            // option to its own schedule.
            ("SUAllowsAutomaticUpdates", Value::Boolean(true)),
            // No system profile rides along with a feed request. Off is
            // Sparkle's default; spelled out because the promise depends on it.
            ("SUEnableSystemProfiling", Value::Boolean(false)),
            // Check the EdDSA signature before unpacking the download, not
            // after. The DMG's Developer ID signature remains the fallback
            // Sparkle allows for rotating the key.
            ("SUVerifyUpdateBeforeExtraction", Value::Boolean(true)),
        ]
    }
}

/// `Sparkle.framework` inside `app`.
pub(super) fn framework(app: &Path) -> PathBuf {
    app.join("Contents/Frameworks/Sparkle.framework")
}

/// Sparkle's code in the inside-out order `codesign` must seal it, each with
/// the hardened runtime and no `--deep` (Sparkle's own instructions); empty
/// when `app` embeds no Sparkle.
pub(super) fn code_inside_out(app: &Path) -> Vec<PathBuf> {
    let framework = framework(app);
    if !framework.exists() {
        return Vec::new();
    }
    let version = framework.join("Versions").join(FRAMEWORK_VERSION);
    vec![
        version.join("Autoupdate"),
        version.join("Updater.app"),
        framework,
    ]
}

/// Embed the pinned Sparkle.framework into `app` and point it at `feed`.
/// Runs before signing: the bundle's signature seals both.
pub(super) fn embed(root: &Path, app: &Path, feed: &Feed) -> Result<()> {
    println!("==> Sparkle {VERSION} ({})", feed.url);
    let release = fetch_release(root)?;
    let source = release.join("Sparkle.framework");
    let framework = framework(app);
    if framework.exists() {
        fs_err::remove_dir_all(&framework)
            .with_context(|| format!("could not remove {}", framework.display()))?;
    }
    if let Some(frameworks) = framework.parent() {
        fs_err::create_dir_all(frameworks)
            .with_context(|| format!("could not create {}", frameworks.display()))?;
    }
    // `ditto` keeps the framework's version symlinks intact.
    let sh = Shell::new()?;
    cmd!(sh, "ditto {source} {framework}").run()?;

    let version = framework.join("Versions").join(FRAMEWORK_VERSION);
    ensure!(
        version.join("Sparkle").is_file(),
        "Sparkle {VERSION} has no Versions/{FRAMEWORK_VERSION}/Sparkle; update FRAMEWORK_VERSION"
    );
    // OpenLogi is not sandboxed, so Sparkle never uses its XPC services, and
    // its sandboxing guide has non-sandboxed apps remove them rather than
    // sign code that cannot run.
    fs_err::remove_file(framework.join("XPCServices"))
        .context("could not remove Sparkle's XPCServices link")?;
    fs_err::remove_dir_all(version.join("XPCServices"))
        .context("could not remove Sparkle's XPC services")?;
    // Sparkle is MIT-licensed; its notice travels with the framework.
    fs_err::copy(release.join("LICENSE"), version.join("Resources/LICENSE"))
        .context("could not copy Sparkle's license")?;
    configure(&app.join("Contents/Info.plist"), feed)
}

/// Stamp `feed`'s keys into `info_plist` and read every one back, after
/// checking the bundle version Sparkle will compare.
fn configure(info_plist: &Path, feed: &Feed) -> Result<()> {
    // Sparkle offers an update only when the appcast's `sparkle:version` — the
    // release version — is newer than `CFBundleVersion`. cargo-bundle's build
    // timestamp there would outrank every release, so no update would ever be
    // offered.
    let bundle_version = info_plist::read_plist_string(info_plist, "CFBundleVersion")?;
    let release_version = info_plist::read_plist_string(info_plist, "CFBundleShortVersionString")?;
    ensure!(
        bundle_version.is_some() && bundle_version == release_version,
        "{} has CFBundleVersion {bundle_version:?}; Sparkle needs the release version {release_version:?} there",
        info_plist.display()
    );
    let entries = feed.info_plist_entries();
    info_plist::stamp_plist_values(info_plist, &entries)?;
    for (key, expected) in &entries {
        let actual = info_plist::read_plist_value(info_plist, key)?;
        ensure!(
            actual.as_ref() == Some(expected),
            "{} has {key} = {actual:?}, expected {expected:?}",
            info_plist.display()
        );
    }
    Ok(())
}

/// Unpack the pinned Sparkle release under `target/sparkle`, downloading it
/// first unless a copy with the pinned hash is already there. Unpacks afresh
/// every time, so nothing left in that directory can reach the bundle.
fn fetch_release(root: &Path) -> Result<PathBuf> {
    let dir = root.join("target/sparkle");
    fs_err::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let archive = dir.join(format!("Sparkle-{VERSION}.tar.xz"));
    let sh = Shell::new()?;
    if !archive.is_file() || archive_sha256(&archive)? != ARCHIVE_SHA256 {
        let url = format!(
            "https://github.com/sparkle-project/Sparkle/releases/download/{VERSION}/Sparkle-{VERSION}.tar.xz"
        );
        cmd!(sh, "curl -fsSL --retry 3 {url} -o {archive}")
            .run()
            .with_context(|| format!("failed to download {url}"))?;
        let actual = archive_sha256(&archive)?;
        ensure!(
            actual == ARCHIVE_SHA256,
            "{} has SHA-256 {actual}, expected {ARCHIVE_SHA256}",
            archive.display()
        );
    }

    let unpacked = dir.join(format!("Sparkle-{VERSION}"));
    if unpacked.exists() {
        fs_err::remove_dir_all(&unpacked)
            .with_context(|| format!("could not remove {}", unpacked.display()))?;
    }
    fs_err::create_dir_all(&unpacked)
        .with_context(|| format!("could not create {}", unpacked.display()))?;
    cmd!(sh, "tar -xJf {archive} -C {unpacked}").run()?;
    Ok(unpacked)
}

fn archive_sha256(archive: &Path) -> Result<String> {
    archive
        .sha256()
        .with_context(|| format!("could not hash {}", archive.display()))
}

#[cfg(test)]
mod tests;

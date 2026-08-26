//! Assembling `OpenLogi.app`: the order the pieces go in, and why.

mod embed;
pub(crate) mod identity;
mod signing;

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::ValueEnum;
use strum::VariantArray as _;
use xshell::{Shell, cmd};

use crate::icon::IconPipeline as _;
use crate::icon::macos::AppBundle;
use crate::support::fs::{command_exists, ensure_dir, repo_root};
use crate::support::info_plist;
use crate::support::xcode;
use identity::{Channel, Component};

// The rest of the macOS domain reaches these through `bundle::`, which is the
// module that owns them conceptually even now that the code sits deeper.
pub(super) use embed::{HELPERS, Helper};
pub(super) use signing::quoted_identity;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum DistributionTarget {
    #[value(name = "aarch64-apple-darwin")]
    Aarch64,
    #[value(name = "x86_64-apple-darwin")]
    X8664,
}

impl DistributionTarget {
    fn triple(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64-apple-darwin",
            Self::X8664 => "x86_64-apple-darwin",
        }
    }
}

/// Build `OpenLogi.app` wearing `channel`'s identity, signing it with whatever
/// local identity is available (dev) or leaving it unsigned (production).
pub(crate) fn run(channel: Channel) -> Result<()> {
    run_with_channel(channel, None, None)
}

/// Build the bundle that ships: always the production identity, signed with the
/// Developer ID identity when one is given.
pub(crate) fn run_for_distribution(
    sign_identity: Option<&str>,
    target: Option<DistributionTarget>,
) -> Result<()> {
    run_with_channel(Channel::Production, sign_identity, target)
}

fn run_with_channel(
    channel: Channel,
    sign_identity: Option<&str>,
    target: Option<DistributionTarget>,
) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);
    let xcode_env = xcode::env()?;
    let target_triple = target.map(DistributionTarget::triple);
    let release_dir = release_dir(&root, target_triple);

    println!("==> app icon");
    AppBundle.compile()?;

    if env::var("OPENLOGI_BUNDLE_ASSETS").as_deref() == Ok("1") {
        println!("==> device assets: bundling (offline build)");
        cmd!(sh, "cargo run -p openlogi --release -- assets sync")
            .envs(xcode_env.iter().map(|(key, value)| (key, value)))
            .run()?;
    } else {
        println!("==> device assets: on-demand (not bundled; fetched at first launch)");
        let assets = root.join("crates/openlogi-desktop/assets");
        if assets.exists() {
            fs_err::remove_dir_all(&assets)
                .with_context(|| format!("could not remove {}", assets.display()))?;
        }
        fs_err::create_dir_all(&assets)
            .with_context(|| format!("could not create {}", assets.display()))?;
    }

    println!("==> bundle (.app)");
    if !command_exists("cargo-bundle") {
        cmd!(sh, "cargo install cargo-bundle --locked")
            .env("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", "/usr/bin/cc")
            .envs(xcode_env.iter().map(|(key, value)| (key, value)))
            .run()?;
    }
    embed::build_release_binaries(&root, &xcode_env, target_triple)?;
    let target_args: Vec<&str> = target_triple
        .into_iter()
        .flat_map(|triple| ["--target", triple])
        .collect();
    cmd!(
        sh,
        "cargo bundle --release --package openlogi-desktop --bin openlogi-desktop --format osx {target_args...}"
    )
    .env("CARGO_BUNDLE_SKIP_BUILD", "1")
    .envs(xcode_env.iter().map(|(key, value)| (key, value)))
    .run()?;

    let built_app = release_dir.join("bundle/osx/OpenLogi.app");
    ensure_dir(&built_app)?;
    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    move_to_canonical_path(&built_app, &app)?;
    AppBundle.install(&app)?;
    embed::embed_helpers(&root, &release_dir, &app, channel)?;
    embed::embed_cli(&release_dir, &app)?;
    embed::verify_bundle_binaries(&app, channel)?;
    info_plist::stamp_privacy_usage_descriptions(&app)?;
    // Identity first, then the checks, then signing — a signature seals the
    // `Info.plist` files, so nothing may rewrite them afterwards.
    identity::stamp(&app, channel, Component::VARIANTS)?;
    identity::verify(&app, channel, Component::VARIANTS)?;
    identity::verify_icons(&app, channel, Component::VARIANTS)?;
    AppBundle.verify(&app)?;
    match (channel, sign_identity) {
        (Channel::Production, Some(identity)) => {
            signing::sign_app_with_timestamp(identity, signing::TimestampMode::Secure, channel)?;
        }
        (Channel::Production, None) => {
            println!("==> codesign: skipped (unsigned — set OPENLOGI_SIGN_IDENTITY to sign)");
        }
        (Channel::Dev, _) => signing::local_sign_app_if_available(channel)?,
    }
    println!();
    println!("Bundle ready: {}", app.display());
    Ok(())
}

fn release_dir(root: &Path, target: Option<&str>) -> PathBuf {
    let mut dir = root.join("target");
    if let Some(target) = target {
        dir.push(target);
    }
    dir.join("release")
}

fn move_to_canonical_path(source: &Path, destination: &Path) -> Result<()> {
    if source == destination {
        return Ok(());
    }
    if destination.exists() {
        fs_err::remove_dir_all(destination)
            .with_context(|| format!("could not remove {}", destination.display()))?;
    }
    if let Some(parent) = destination.parent() {
        fs_err::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs_err::rename(source, destination).with_context(|| {
        format!(
            "could not move {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bundle_already_has_the_canonical_path() {
        let root = tempfile::tempdir().unwrap();
        let app = release_dir(root.path(), None).join("bundle/osx/OpenLogi.app");
        fs_err::create_dir_all(&app).unwrap();
        fs_err::write(app.join("binary"), []).unwrap();

        assert_eq!(
            app,
            root.path().join("target/release/bundle/osx/OpenLogi.app")
        );
        move_to_canonical_path(&app, &app).unwrap();
        assert!(app.join("binary").is_file());
    }

    #[test]
    fn cross_compiled_bundle_replaces_the_canonical_app() {
        let root = tempfile::tempdir().unwrap();
        let source =
            release_dir(root.path(), Some("x86_64-apple-darwin")).join("bundle/osx/OpenLogi.app");
        let destination = root.path().join("target/release/bundle/osx/OpenLogi.app");
        fs_err::create_dir_all(&source).unwrap();
        fs_err::write(source.join("new"), []).unwrap();
        fs_err::create_dir_all(&destination).unwrap();
        fs_err::write(destination.join("stale"), []).unwrap();

        move_to_canonical_path(&source, &destination).unwrap();

        assert!(!source.exists());
        assert!(destination.join("new").is_file());
        assert!(!destination.join("stale").exists());
    }
}

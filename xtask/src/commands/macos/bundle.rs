//! Assembling `OpenLogi.app`: the order the pieces go in, and why.

mod app_icon;
mod embed;
pub(crate) mod identity;
mod info_plist;
mod signing;

use std::env;
use std::path::Path;

use anyhow::{Context as _, Result};
use strum::VariantArray as _;
use xshell::{Shell, cmd};

use crate::support::fs::{command_exists, ensure_dir, repo_root};
use identity::{Channel, Component};

// The rest of the macOS domain reaches these through `bundle::`, which is the
// module that owns them conceptually even now that the code sits deeper.
pub(crate) use app_icon::{generate_app_icon, install_app_icon};
pub(super) use embed::{HELPERS, Helper};
pub(super) use signing::quoted_identity;

/// Build `OpenLogi.app` wearing `channel`'s identity, signing it with whatever
/// local identity is available (dev) or leaving it unsigned (production).
pub(crate) fn run(channel: Channel) -> Result<()> {
    run_with_channel(channel, None)
}

/// Build the bundle that ships: always the production identity, signed with the
/// Developer ID identity when one is given.
pub(crate) fn run_for_distribution(sign_identity: Option<&str>) -> Result<()> {
    run_with_channel(Channel::Production, sign_identity)
}

fn run_with_channel(channel: Channel, sign_identity: Option<&str>) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);
    let xcode_env = xcode_env()?;

    println!("==> app icon");
    generate_app_icon()?;

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
    {
        let gui_dir = root.join("crates/openlogi-desktop");
        let _gui = sh.push_dir(gui_dir);
        cmd!(sh, "cargo bundle --release")
            .envs(xcode_env.iter().map(|(key, value)| (key, value)))
            .run()?;
    }
    remove_cargo_bundle_dmg(&root)?;

    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    ensure_dir(&app)?;
    install_app_icon(&app)?;
    embed::embed_helpers(&root, &app, &xcode_env, channel)?;
    embed::embed_cli(&root, &app, &xcode_env)?;
    embed::verify_bundle_binaries(&app, channel)?;
    info_plist::stamp_privacy_usage_descriptions(&app)?;
    // Identity first, then the checks, then signing — a signature seals the
    // `Info.plist` files, so nothing may rewrite them afterwards.
    identity::stamp(&app, channel, Component::VARIANTS)?;
    identity::verify(&app, channel, Component::VARIANTS)?;
    identity::verify_icons(&app, channel, Component::VARIANTS)?;
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

fn remove_cargo_bundle_dmg(root: &Path) -> Result<()> {
    let dmg = root.join("target/release/bundle/dmg/OpenLogi.dmg");
    if dmg.exists() {
        fs_err::remove_file(&dmg)
            .with_context(|| format!("could not remove stale {}", dmg.display()))?;
        println!(
            "    removed cargo-bundle DMG before helper embedding; use `macos package` for a DMG"
        );
    }
    Ok(())
}

pub(super) fn xcode_env() -> Result<Vec<(String, String)>> {
    let sh = Shell::new()?;
    let developer_dir = env::var("OPENLOGI_DEVELOPER_DIR")
        .unwrap_or_else(|_| "/Applications/Xcode.app/Contents/Developer".to_string());
    let sdkroot = cmd!(sh, "/usr/bin/xcrun --sdk macosx --show-sdk-path")
        .env("DEVELOPER_DIR", &developer_dir)
        .read()?;
    Ok(vec![
        ("DEVELOPER_DIR".to_string(), developer_dir),
        ("SDKROOT".to_string(), sdkroot.trim().to_string()),
    ])
}

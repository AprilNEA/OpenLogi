//! Signing the shipped bundle, inside out.
//!
//! Which identity is used and whether it carries a secure timestamp is the
//! difference between a build that notarizes and one that only runs locally,
//! so both are decided here rather than at the call site.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::Result;
use xshell::{Shell, cmd};

use super::identity::{Channel, Component};
use crate::support::fs::{ensure_file, repo_root};

pub(super) fn local_sign_app_if_available(channel: Channel) -> Result<()> {
    if env::var("OPENLOGI_LOCAL_CODESIGN").as_deref() == Ok("0") {
        println!("==> local codesign: skipped (OPENLOGI_LOCAL_CODESIGN=0)");
        return Ok(());
    }

    if let Some(identity) = env_nonempty("OPENLOGI_SIGN_IDENTITY") {
        sign_app_with_timestamp(&identity, TimestampMode::Secure, channel)?;
        return Ok(());
    }

    if let Some(identity) = env_nonempty("OPENLOGI_LOCAL_CODESIGN_IDENTITY") {
        sign_app_with_timestamp(&identity, TimestampMode::None, channel)?;
        return Ok(());
    }

    if let Some(identity) = first_apple_development_identity()? {
        sign_app_with_timestamp(&identity, TimestampMode::None, channel)?;
        return Ok(());
    }

    println!(
        "==> local codesign: skipped (no Apple Development identity found;          set OPENLOGI_LOCAL_CODESIGN_IDENTITY or OPENLOGI_SIGN_IDENTITY to sign)"
    );
    println!(
        "    warning: an unsigned bundle is re-signed ad-hoc on every build, so its own Accessibility grant goes stale each time"
    );
    Ok(())
}

pub(super) fn sign_app_with_timestamp(
    identity: &str,
    timestamp: TimestampMode,
    channel: Channel,
) -> Result<()> {
    let sh = Shell::new()?;
    let root = repo_root()?;
    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    let helper = Component::Agent.root(&app, channel);
    let overlay = Component::Overlay.root(&app, channel);
    // GUI + embedded CLI open the camera (preview / snapshot). The agent and
    // overlay helpers do not — leave them without camera entitlements.
    let camera_ents = camera_entitlements_path(&root);
    ensure_file(&camera_ents)?;
    println!("==> codesign ({identity})");
    // Inside-out signing: seal the nested helper with its own signature first,
    // then the outer app (which seals the already-signed helper). `--deep` is
    // deprecated and can't give the helper an independent signature — but a
    // stable, separately-signed helper identity is exactly what lets the agent's
    // Accessibility (TCC) grant persist across updates. So sign each explicitly.
    if helper.exists() {
        codesign_runtime(identity, &helper, timestamp, None)?;
    }
    if overlay.exists() {
        codesign_runtime(identity, &overlay, timestamp, None)?;
    }
    // The embedded CLI is a second Mach-O under Contents/MacOS; sign it with the
    // hardened runtime before the outer app so it carries a Developer ID
    // signature (its as-built ad-hoc signature would fail notarization).
    let cli = app.join("Contents/MacOS/openlogi");
    if cli.exists() {
        codesign_runtime(identity, &cli, timestamp, Some(&camera_ents))?;
    }
    codesign_runtime(identity, &app, timestamp, Some(&camera_ents))?;
    cmd!(sh, "codesign --verify --strict {app}").run()?;
    if helper.exists() {
        cmd!(sh, "codesign --verify --strict {helper}").run()?;
    }
    if overlay.exists() {
        cmd!(sh, "codesign --verify --strict {overlay}").run()?;
    }
    if cli.exists() {
        cmd!(sh, "codesign --verify --strict {cli}").run()?;
    }
    Ok(())
}

/// Path to the GUI/CLI entitlements (camera hardened-runtime exception).
fn camera_entitlements_path(root: &Path) -> PathBuf {
    root.join("crates/openlogi-desktop/bundle/OpenLogi.entitlements")
}

/// Sign one target with the hardened runtime and the requested timestamp mode.
fn codesign_runtime(
    identity: &str,
    target: &Path,
    timestamp: TimestampMode,
    entitlements: Option<&Path>,
) -> Result<()> {
    let sh = Shell::new()?;
    match (timestamp, entitlements) {
        (TimestampMode::Secure, Some(ents)) => {
            cmd!(
                sh,
                "codesign --force --options runtime --timestamp --entitlements {ents} --sign {identity} {target}"
            )
            .run()?;
        }
        (TimestampMode::Secure, None) => {
            cmd!(
                sh,
                "codesign --force --options runtime --timestamp --sign {identity} {target}"
            )
            .run()?;
        }
        (TimestampMode::None, Some(ents)) => {
            cmd!(
                sh,
                "codesign --force --options runtime --timestamp=none --entitlements {ents} --sign {identity} {target}"
            )
            .run()?;
        }
        (TimestampMode::None, None) => {
            cmd!(
                sh,
                "codesign --force --options runtime --timestamp=none --sign {identity} {target}"
            )
            .run()?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum TimestampMode {
    Secure,
    None,
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn first_apple_development_identity() -> Result<Option<String>> {
    let sh = Shell::new()?;
    let Ok(output) = cmd!(sh, "security find-identity -v -p codesigning").read() else {
        return Ok(None);
    };
    Ok(output
        .lines()
        .filter_map(quoted_identity)
        .find(|identity| identity.starts_with("Apple Development:")))
}

pub(crate) fn quoted_identity(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_string())
}

#[cfg(test)]
mod tests;

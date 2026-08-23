//! What goes inside the finished `.app`: the login-item helpers, the CLI, and
//! the check that every Mach-O the bundle promises is actually there.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

use super::identity::{Channel, Component};
use crate::support::fs::ensure_file;
use crate::support::info_plist::stamp_bundle_version;

/// A nested login-item helper embedded under `Contents/Library/LoginItems`.
pub(crate) struct Helper {
    /// Identity component, which also locates the helper inside the app bundle.
    pub(crate) component: Component,
    /// Cargo package that builds it.
    pub(crate) package: &'static str,
    /// Binary name, both in the profile directory and inside the helper bundle.
    pub(crate) binary: &'static str,
    /// Checked-in `Info.plist` template, relative to the repo root. It carries
    /// the shipped identity; [`super::identity::stamp`] writes the building channel's
    /// over it, so the dev bundle needs no template of its own.
    pub(crate) info_plist: &'static str,
    /// What the build log calls it.
    pub(crate) label: &'static str,
}

/// Every helper the app bundle ships.
pub(crate) const HELPERS: [Helper; 2] = [
    Helper {
        component: Component::Agent,
        package: "openlogi-agent",
        binary: "openlogi-agent",
        info_plist: "crates/openlogi-desktop/bundle/agent-release/Info.plist",
        label: "agent helper",
    },
    Helper {
        component: Component::Overlay,
        package: "openlogi-overlay",
        binary: "openlogi-overlay",
        info_plist: "crates/openlogi-desktop/bundle/overlay-release/Info.plist",
        label: "Actions Ring overlay helper",
    },
];

/// Build each helper and embed it as a nested login-item bundle.
///
/// The agent is the always-on process (hook + device I/O + menu bar); shipping
/// it inside the GUI bundle keeps one notarized artifact, lets `open -b`
/// foreground the GUI from the agent's menu, and gives the agent a stable
/// signed identity so its Accessibility (TCC) grant survives app updates.
///
/// Every helper gets the GUI's icon, so each shows the OpenLogi mark rather than
/// a generic blank wherever macOS lists it — System Settings' Accessibility
/// pane, Login Items. Icon generation already ran, so the icns is on disk.
pub(super) fn embed_helpers(
    root: &Path,
    app: &Path,
    xcode_env: &[(String, String)],
    channel: Channel,
) -> Result<()> {
    let icon = root.join("crates/openlogi-desktop/icon/AppIcon.icns");
    ensure_file(&icon)?;
    for helper in &HELPERS {
        embed_helper(root, app, xcode_env, helper, &icon, channel)?;
    }
    Ok(())
}

fn embed_helper(
    root: &Path,
    app: &Path,
    xcode_env: &[(String, String)],
    helper: &Helper,
    icon: &Path,
    channel: Channel,
) -> Result<()> {
    let sh = Shell::new()?;
    let _repo = sh.push_dir(root);
    let Helper {
        package,
        binary,
        label,
        ..
    } = *helper;
    println!("==> {label} (build)");
    cmd!(sh, "cargo build -p {package} --bin {binary} --release")
        .envs(xcode_env.iter().map(|(key, value)| (key, value)))
        .run()?;
    let built = root.join("target/release").join(binary);
    ensure_file(&built)?;

    let bundle = helper.component.root(app, channel);
    let bundle_macos = bundle.join("Contents/MacOS");
    fs_err::create_dir_all(&bundle_macos)
        .with_context(|| format!("could not create {}", bundle_macos.display()))?;
    fs_err::copy(&built, bundle_macos.join(binary))
        .with_context(|| format!("could not copy {binary} into the helper bundle"))?;

    let info_src = root.join(helper.info_plist);
    ensure_file(&info_src)?;
    let info_dst = helper.component.info_plist(app, channel);
    fs_err::copy(&info_src, &info_dst)
        .with_context(|| format!("could not write the {label} Info.plist"))?;
    stamp_bundle_version(&info_dst, env!("CARGO_PKG_VERSION"))?;

    let resources = bundle.join("Contents/Resources");
    fs_err::create_dir_all(&resources)
        .with_context(|| format!("could not create {}", resources.display()))?;
    fs_err::copy(icon, helper.component.icon(app, channel))
        .with_context(|| format!("could not copy the app icon into the {label} bundle"))?;

    println!("    embedded {}", bundle.display());
    Ok(())
}

pub(super) fn embed_cli(root: &Path, app: &Path, xcode_env: &[(String, String)]) -> Result<()> {
    let sh = Shell::new()?;
    let _repo = sh.push_dir(root);
    println!("==> cli (build)");
    cmd!(sh, "cargo build -p openlogi --release")
        .envs(xcode_env.iter().map(|(key, value)| (key, value)))
        .run()?;
    let cli_bin = root.join("target/release/openlogi");
    ensure_file(&cli_bin)?;

    let macos = app.join("Contents/MacOS");
    fs_err::copy(&cli_bin, macos.join("openlogi"))
        .with_context(|| "could not copy the CLI binary into the app bundle".to_string())?;

    println!("    embedded {}", macos.join("openlogi").display());
    Ok(())
}

/// Every Mach-O the finished bundle must ship, for `channel`'s helper layout.
fn required_bundle_binaries(app: &Path, channel: Channel) -> Vec<PathBuf> {
    let macos = app.join("Contents/MacOS");
    let mut required = vec![macos.join("openlogi"), macos.join("openlogi-desktop")];
    required.extend(HELPERS.iter().map(|helper| {
        helper
            .component
            .root(app, channel)
            .join("Contents/MacOS")
            .join(helper.binary)
    }));
    required
}

pub(super) fn verify_bundle_binaries(app: &Path, channel: Channel) -> Result<()> {
    for path in required_bundle_binaries(app, channel) {
        ensure_file(&path)
            .with_context(|| format!("missing required bundle binary {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

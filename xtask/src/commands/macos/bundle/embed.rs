//! What goes inside the finished `.app`: the login-item helpers, the CLI, the
//! agent's launchd service plist, and the check that every Mach-O the bundle
//! promises is actually there.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use openlogi_core::brand;
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

/// Build every executable the distribution bundle contains in one Cargo
/// invocation so their shared dependency features are unified once.
pub(super) fn build_release_binaries(
    root: &Path,
    xcode_env: &[(String, String)],
    target: Option<&str>,
) -> Result<()> {
    let sh = Shell::new()?;
    let _repo = sh.push_dir(root);
    let mut targets = vec!["--package", "openlogi-desktop", "--bin", "openlogi-desktop"];
    for helper in &HELPERS {
        targets.extend(["--package", helper.package, "--bin", helper.binary]);
    }
    targets.extend(["--package", "openlogi", "--bin", "openlogi"]);
    if let Some(target) = target {
        targets.extend(["--target", target]);
    }

    println!("==> release binaries (build)");
    cmd!(sh, "cargo build --locked --release {targets...}")
        .envs(xcode_env.iter().map(|(key, value)| (key, value)))
        .run()?;
    Ok(())
}

/// Embed each helper as a nested login-item bundle.
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
    release_dir: &Path,
    app: &Path,
    channel: Channel,
) -> Result<()> {
    let icon = root.join("crates/openlogi-desktop/icon/AppIcon.icns");
    ensure_file(&icon)?;
    for helper in &HELPERS {
        embed_helper(root, release_dir, app, helper, &icon, channel)?;
    }
    Ok(())
}

fn embed_helper(
    root: &Path,
    release_dir: &Path,
    app: &Path,
    helper: &Helper,
    icon: &Path,
    channel: Channel,
) -> Result<()> {
    let Helper { binary, label, .. } = *helper;
    println!("==> {label} (embed)");
    let built = release_dir.join(binary);
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

/// The launchd service label `channel`'s bundle carries — what its embedded
/// LaunchAgent plist declares, `SMAppService` registers, and `launchctl`
/// addresses. Frozen once shipped; see [`brand::AGENT_SERVICE_LABEL`].
pub(crate) fn agent_service_label(channel: Channel) -> String {
    match channel {
        Channel::Production => brand::AGENT_SERVICE_LABEL.to_owned(),
        Channel::Dev => brand::dev_id(brand::AGENT_SERVICE_LABEL),
    }
}

/// The launchd property list `SMAppService` registers the agent from.
///
/// `BundleProgram` (not `Program`) so launchd resolves the helper relative to
/// wherever the app bundle lives — the registration survives the user moving
/// the app. The key exists only in the `SMAppService` layer, which supplies
/// the bundle context: a raw `launchctl bootstrap` of this file fails with
/// `Input/output error` (verified) — registration through the framework is
/// the sole loading path. `KeepAlive = {SuccessfulExit: false}` is the
/// supervision contract: a crash is respawned, the tray's Quit (a clean
/// `exit(0)`) stays down. Per launchd.plist(5), `SuccessfulExit` implies
/// `RunAtLoad`, so a registered service also starts at login and immediately
/// upon registration — and an explicit `RunAtLoad = false` does not override
/// the implication (verified: the job still runs once at load).
///
/// `SuccessfulExit` is deliberate over the narrower `KeepAlive = {Crashed:
/// true}`: this plist wants the implied autostart anyway, and `SuccessfulExit`
/// respawns every failure mode — a Rust panic under the default
/// `panic = "unwind"` is an `exit(101)`, not a signal, which `Crashed` would
/// leave down (verified live, alongside `Crashed` not respawning SIGKILL).
/// `Crashed` *does* compose with `RunAtLoad = false`, so a supervised
/// on-demand variant for `launch_at_login = false` is representable — a
/// possible second service plist later, not shipped here.
fn agent_launch_plist(channel: Channel) -> Result<plist::Dictionary> {
    let helper = HELPERS
        .iter()
        .find(|helper| helper.component == Component::Agent)
        .ok_or_else(|| anyhow!("HELPERS carries no agent entry"))?;
    let nested = Component::Agent
        .nested_bundle(channel)
        .ok_or_else(|| anyhow!("the agent component is always a nested bundle"))?;

    let mut keep_alive = plist::Dictionary::new();
    keep_alive.insert("SuccessfulExit".into(), plist::Value::Boolean(false));
    let mut root = plist::Dictionary::new();
    root.insert(
        "Label".into(),
        plist::Value::String(agent_service_label(channel)),
    );
    root.insert(
        "BundleProgram".into(),
        plist::Value::String(format!("{nested}/Contents/MacOS/{}", helper.binary)),
    );
    root.insert("KeepAlive".into(), plist::Value::Dictionary(keep_alive));
    Ok(root)
}

/// Write `channel`'s agent service plist into the app bundle at
/// `Contents/Library/LaunchAgents/<label>.plist` — the location
/// `SMAppService.agent(plistName:)` resolves against. Must run after the
/// helpers are embedded (the `BundleProgram` target is verified to exist) and
/// before signing (the app signature seals `Contents`).
pub(crate) fn write_agent_launch_plist(app: &Path, channel: Channel) -> Result<()> {
    let content = agent_launch_plist(channel)?;
    if let Some(plist::Value::String(bundle_program)) = content.get("BundleProgram") {
        ensure_file(&app.join(bundle_program))
            .context("the agent service plist must be written after the helpers are embedded")?;
    }
    let dir = app.join("Contents/Library/LaunchAgents");
    fs_err::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let path = dir.join(format!("{}.plist", agent_service_label(channel)));
    plist::Value::Dictionary(content)
        .to_file_xml(&path)
        .with_context(|| format!("could not write {}", path.display()))?;
    println!("    wrote {}", path.display());
    Ok(())
}

pub(super) fn embed_cli(release_dir: &Path, app: &Path) -> Result<()> {
    println!("==> cli (embed)");
    let cli_bin = release_dir.join("openlogi");
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

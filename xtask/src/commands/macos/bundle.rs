pub(crate) mod identity;

use std::env;
use std::path::Path;

use anyhow::{Context as _, Result};
use plist::Value;
use xshell::{Shell, cmd};

use strum::VariantArray as _;

use crate::support::fs::{command_exists, ensure_dir, ensure_file, repo_root};
use identity::{Channel, Component};

pub(crate) fn generate_icns() -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let master = root.join("design/icon/openlogi.png");
    let output_dir = root.join("crates/openlogi-desktop/icon");
    let output = output_dir.join("AppIcon.icns");

    ensure_file(&master)?;
    fs_err::create_dir_all(&output_dir).with_context(|| {
        format!(
            "could not create icon output directory {}",
            output_dir.display()
        )
    })?;

    let work = tempfile::Builder::new()
        .prefix("openlogi-icns-")
        .tempdir()
        .context("could not create temporary iconset directory")?;
    let iconset = work.path().join("AppIcon.iconset");
    fs_err::create_dir_all(&iconset)
        .with_context(|| format!("could not create iconset directory {}", iconset.display()))?;

    render_iconset(&iconset, |size, output| {
        let size = size.to_string();
        cmd!(sh, "sips -z {size} {size} {master} --out {output}")
            .ignore_stdout()
            .run()?;
        Ok(())
    })?;

    // Let Apple's encoder choose the ICNS chunk layout. The Rust `icns` crate
    // emits `icp4`/`icp5` PNG chunks that current macOS releases decode as
    // corrupted pixels in small-icon surfaces such as Login Items.
    cmd!(sh, "iconutil -c icns {iconset} -o {output}").run()?;
    println!("wrote {}", output.display());
    Ok(())
}

fn render_iconset<F>(iconset: &Path, mut render: F) -> Result<()>
where
    F: FnMut(u16, &Path) -> Result<()>,
{
    for size in [16, 32, 128, 256, 512] {
        render(size, &iconset.join(format!("icon_{size}x{size}.png")))?;
        render(
            size * 2,
            &iconset.join(format!("icon_{size}x{size}@2x.png")),
        )?;
    }
    Ok(())
}

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
    generate_icns()?;

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
    embed_helpers(&root, &app, &xcode_env, channel)?;
    embed_cli(&root, &app, &xcode_env)?;
    verify_bundle_binaries(&app, channel)?;
    stamp_privacy_usage_descriptions(&app)?;
    // Identity first, then the checks, then signing — a signature seals the
    // `Info.plist` files, so nothing may rewrite them afterwards.
    identity::stamp(&app, channel, Component::VARIANTS)?;
    identity::verify(&app, channel, Component::VARIANTS)?;
    identity::verify_icons(&app, channel, Component::VARIANTS)?;
    match (channel, sign_identity) {
        (Channel::Production, Some(identity)) => {
            sign_app_with_timestamp(identity, TimestampMode::Secure, channel)?;
        }
        (Channel::Production, None) => {
            println!("==> codesign: skipped (unsigned — set OPENLOGI_SIGN_IDENTITY to sign)");
        }
        (Channel::Dev, _) => local_sign_app_if_available(channel)?,
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

/// A nested login-item helper embedded under `Contents/Library/LoginItems`.
pub(super) struct Helper {
    /// Identity component, which also locates the helper inside the app bundle.
    pub(super) component: Component,
    /// Cargo package that builds it.
    pub(super) package: &'static str,
    /// Binary name, both in the profile directory and inside the helper bundle.
    pub(super) binary: &'static str,
    /// Checked-in `Info.plist` template, relative to the repo root. It carries
    /// the shipped identity; [`identity::stamp`] writes the building channel's
    /// over it, so the dev bundle needs no template of its own.
    pub(super) info_plist: &'static str,
    /// What the build log calls it.
    pub(super) label: &'static str,
}

/// Every helper the app bundle ships.
pub(super) const HELPERS: [Helper; 2] = [
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
fn embed_helpers(
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

fn embed_cli(root: &Path, app: &Path, xcode_env: &[(String, String)]) -> Result<()> {
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
fn required_bundle_binaries(app: &Path, channel: Channel) -> Vec<std::path::PathBuf> {
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

fn verify_bundle_binaries(app: &Path, channel: Channel) -> Result<()> {
    for path in required_bundle_binaries(app, channel) {
        ensure_file(&path)
            .with_context(|| format!("missing required bundle binary {}", path.display()))?;
    }
    Ok(())
}

/// Stamp `NSCameraUsageDescription` (cargo-bundle can't; matches the dev plist) so camera requests prompt instead of killing the app.
fn stamp_privacy_usage_descriptions(app: &Path) -> Result<()> {
    println!("==> privacy usage descriptions");
    stamp_plist_strings(
        &app.join("Contents/Info.plist"),
        &[(
            "NSCameraUsageDescription",
            "OpenLogi previews your Logitech webcam locally. Video never leaves your Mac.",
        )],
    )
}

pub(super) fn stamp_bundle_version(info_plist: &Path, version: &str) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        dict.insert(key.into(), Value::String(version.to_string()));
    }
    plist
        .to_file_xml(info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
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

/// Read one string value from an `Info.plist`; `None` when the key is absent.
fn read_plist_string(info_plist: &Path, key: &str) -> Result<Option<String>> {
    let plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    Ok(dict.get(key).and_then(Value::as_string).map(str::to_owned))
}

fn stamp_plist_strings(info_plist: &Path, entries: &[(&str, &str)]) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for (key, value) in entries {
        dict.insert((*key).into(), Value::String((*value).to_string()));
    }
    plist
        .to_file_xml(info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
}

fn local_sign_app_if_available(channel: Channel) -> Result<()> {
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

fn sign_app_with_timestamp(
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
fn camera_entitlements_path(root: &Path) -> std::path::PathBuf {
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
enum TimestampMode {
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

pub(super) fn quoted_identity(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identity work iterates every `Component`, so a component added without a
    /// `Helper` to embed it would only surface as a stamping failure during a
    /// real build.
    #[test]
    fn every_nested_component_is_embedded_by_a_helper() {
        for &component in Component::VARIANTS {
            assert!(
                component == Component::App
                    || HELPERS.iter().any(|helper| helper.component == component),
                "{component} has no Helper entry to embed it"
            );
        }
    }

    fn touch_all(binaries: &[std::path::PathBuf]) {
        for path in binaries {
            fs_err::create_dir_all(path.parent().unwrap()).unwrap();
            fs_err::write(path, b"").unwrap();
        }
    }

    /// Both channels lay their helpers out differently, and both are assembled
    /// by this code — `macos bundle --channel dev` and `macos dev-bundle`.
    #[test]
    fn verify_bundle_binaries_accepts_a_complete_bundle() {
        for channel in [Channel::Production, Channel::Dev] {
            let app = tempfile::tempdir().unwrap();
            touch_all(&required_bundle_binaries(app.path(), channel));

            verify_bundle_binaries(app.path(), channel).unwrap();
        }
    }

    #[test]
    fn camera_entitlements_declare_device_camera() {
        let path = camera_entitlements_path(&repo_root().unwrap());
        let plist = Value::from_file(&path).unwrap();
        let dict = plist.as_dictionary().unwrap();
        assert_eq!(
            dict.get("com.apple.security.device.camera")
                .and_then(Value::as_boolean),
            Some(true),
            "hardened-runtime camera capture needs this entitlement"
        );
    }

    /// The checked-in helper plists are what a fresh bundle starts from, so a
    /// rename there that never reached the identity table would ship one name in
    /// the bundle and another in every verification.
    #[test]
    fn shipped_helper_plists_declare_their_production_identity() {
        let root = repo_root().unwrap();

        for helper in &HELPERS {
            let plist = root.join(helper.info_plist);
            let expected = Channel::Production.identity(helper.component);

            for (key, want) in identity::identity_entries(&expected) {
                assert_eq!(
                    read_plist_string(&plist, key).unwrap().as_deref(),
                    Some(want),
                    "{} declares the wrong {key}",
                    helper.info_plist
                );
            }
        }
    }

    /// Every helper must declare the shared icon, or it shows up blank in the
    /// System Settings panes where users grant it permissions.
    #[test]
    fn shipped_helper_plists_declare_the_shared_icon() {
        let root = repo_root().unwrap();

        for helper in &HELPERS {
            let icon =
                read_plist_string(&root.join(helper.info_plist), "CFBundleIconFile").unwrap();

            assert_eq!(
                icon.as_deref().map(|file| file.trim_end_matches(".icns")),
                Some("AppIcon"),
                "{} must declare the shared app icon",
                helper.info_plist
            );
        }
    }

    #[test]
    fn verify_bundle_binaries_names_each_missing_binary() {
        let channel = Channel::Production;
        let count = required_bundle_binaries(Path::new("/probe"), channel).len();

        for skipped in 0..count {
            let app = tempfile::tempdir().unwrap();
            let required = required_bundle_binaries(app.path(), channel);
            let missing = required[skipped].clone();
            let shipped: Vec<_> = required
                .into_iter()
                .filter(|path| *path != missing)
                .collect();
            touch_all(&shipped);

            let error = verify_bundle_binaries(app.path(), channel).unwrap_err();

            assert!(
                error.to_string().ends_with(&missing.display().to_string()),
                "error should name {}, got: {error}",
                missing.display()
            );
        }
    }
}

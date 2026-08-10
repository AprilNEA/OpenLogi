use std::env;
use std::path::Path;

use anyhow::{Context as _, Result};
use plist::Value;
use xshell::{Shell, cmd};

use crate::support::fs::{command_exists, ensure_dir, ensure_file, repo_root};

pub(crate) fn generate_icns() -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let master = root.join("design/icon/openlogi.png");
    let output_dir = root.join("crates/openlogi-gui/icon");
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

pub(crate) fn run() -> Result<()> {
    run_with_profile(&BundleProfile::Local)
}

pub(crate) fn run_for_distribution(sign_identity: Option<&str>) -> Result<()> {
    run_with_profile(&BundleProfile::Distribution { sign_identity })
}

fn run_with_profile(profile: &BundleProfile<'_>) -> Result<()> {
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
        let assets = root.join("crates/openlogi-gui/assets");
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
        let gui_dir = root.join("crates/openlogi-gui");
        let _gui = sh.push_dir(gui_dir);
        cmd!(sh, "cargo bundle --release")
            .envs(xcode_env.iter().map(|(key, value)| (key, value)))
            .run()?;
    }
    remove_cargo_bundle_dmg(&root)?;

    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    ensure_dir(&app)?;
    embed_agent_helper(&root, &app, &xcode_env)?;
    embed_overlay_helper(&root, &app, &xcode_env)?;
    embed_cli(&root, &app, &xcode_env)?;
    verify_bundle_binaries(&app)?;
    stamp_privacy_usage_descriptions(&app)?;
    match profile {
        BundleProfile::Local => {
            stamp_local_bundle_identity(&app)?;
            local_sign_app_if_available()?;
        }
        BundleProfile::Distribution { sign_identity } => {
            if let Some(identity) = sign_identity {
                sign_app_with_timestamp(identity, TimestampMode::Secure)?;
            }
        }
    }
    println!();
    println!("Bundle ready: {}", app.display());
    Ok(())
}

enum BundleProfile<'a> {
    Local,
    Distribution { sign_identity: Option<&'a str> },
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

/// Build the headless agent and embed it as a nested login-item helper at
/// `OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app`. The agent is
/// the always-on process (hook + device I/O + menu bar); shipping it inside the
/// GUI bundle keeps one notarized artifact, lets `open -b` foreground the GUI
/// from the agent's menu, and gives the agent a stable signed identity so its
/// Accessibility (TCC) grant survives app updates.
fn embed_agent_helper(root: &Path, app: &Path, xcode_env: &[(String, String)]) -> Result<()> {
    let sh = Shell::new()?;
    let _repo = sh.push_dir(root);
    println!("==> agent helper (build)");
    cmd!(sh, "cargo build -p openlogi-agent --release")
        .envs(xcode_env.iter().map(|(key, value)| (key, value)))
        .run()?;
    let agent_bin = root.join("target/release/openlogi-agent");
    ensure_file(&agent_bin)?;

    let helper = app.join("Contents/Library/LoginItems/OpenLogiAgent.app");
    let helper_macos = helper.join("Contents/MacOS");
    fs_err::create_dir_all(&helper_macos)
        .with_context(|| format!("could not create {}", helper_macos.display()))?;
    fs_err::copy(&agent_bin, helper_macos.join("openlogi-agent"))
        .with_context(|| "could not copy the agent binary into the helper bundle".to_string())?;
    let info_src = root.join("crates/openlogi-gui/bundle/agent-release/Info.plist");
    ensure_file(&info_src)?;
    let info_dst = helper.join("Contents/Info.plist");
    fs_err::copy(&info_src, &info_dst)
        .with_context(|| "could not write the helper Info.plist".to_string())?;
    // Share the GUI's app icon so the agent shows the OpenLogi mark (not a
    // generic blank) in System Settings → Accessibility, where the grant now
    // lives under "OpenLogi Agent". The bundle command runs icon generation
    // first, so the icns is already on disk. Matches the Info.plist
    // CFBundleIconFile = "AppIcon".
    let icon_src = root.join("crates/openlogi-gui/icon/AppIcon.icns");
    ensure_file(&icon_src)?;
    let resources = helper.join("Contents/Resources");
    fs_err::create_dir_all(&resources)
        .with_context(|| format!("could not create {}", resources.display()))?;
    fs_err::copy(&icon_src, resources.join("AppIcon.icns"))
        .with_context(|| "could not copy the app icon into the helper bundle".to_string())?;

    stamp_bundle_version(&info_dst, env!("CARGO_PKG_VERSION"))?;

    println!("    embedded {}", helper.display());
    Ok(())
}

fn embed_overlay_helper(root: &Path, app: &Path, xcode_env: &[(String, String)]) -> Result<()> {
    let sh = Shell::new()?;
    let _repo = sh.push_dir(root);
    println!("==> Actions Ring overlay helper (build)");
    cmd!(
        sh,
        "cargo build -p openlogi-gui --bin openlogi-overlay --release"
    )
    .envs(xcode_env.iter().map(|(key, value)| (key, value)))
    .run()?;
    let overlay_bin = root.join("target/release/openlogi-overlay");
    ensure_file(&overlay_bin)?;

    let helper = app.join("Contents/Library/LoginItems/OpenLogiOverlay.app");
    let helper_macos = helper.join("Contents/MacOS");
    fs_err::create_dir_all(&helper_macos)
        .with_context(|| format!("could not create {}", helper_macos.display()))?;
    fs_err::copy(&overlay_bin, helper_macos.join("openlogi-overlay"))
        .with_context(|| "could not copy the Actions Ring overlay binary".to_string())?;
    let info_src = root.join("crates/openlogi-gui/bundle/overlay-release/Info.plist");
    ensure_file(&info_src)?;
    let info_dst = helper.join("Contents/Info.plist");
    fs_err::copy(&info_src, &info_dst)
        .with_context(|| "could not write the overlay helper Info.plist".to_string())?;
    stamp_bundle_version(&info_dst, env!("CARGO_PKG_VERSION"))?;

    println!("    embedded {}", helper.display());
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

/// Every Mach-O the finished bundle must ship, relative to the `.app` root.
const REQUIRED_BUNDLE_BINARIES: [&str; 4] = [
    "Contents/MacOS/openlogi",
    "Contents/MacOS/openlogi-gui",
    "Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
    "Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
];

fn verify_bundle_binaries(app: &Path) -> Result<()> {
    for binary in REQUIRED_BUNDLE_BINARIES {
        let path = app.join(binary);
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

fn stamp_bundle_version(info_plist: &Path, version: &str) -> Result<()> {
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

fn xcode_env() -> Result<Vec<(String, String)>> {
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

fn stamp_local_bundle_identity(app: &Path) -> Result<()> {
    println!("==> local bundle identity");
    let app_info = app.join("Contents/Info.plist");
    stamp_plist_strings(
        &app_info,
        &[
            ("CFBundleDisplayName", "OpenLogi Dev"),
            ("CFBundleIdentifier", "org.openlogi.openlogi.dev"),
            ("CFBundleName", "OpenLogi Dev"),
        ],
    )?;

    let helper_info = app.join("Contents/Library/LoginItems/OpenLogiAgent.app/Contents/Info.plist");
    if helper_info.exists() {
        stamp_plist_strings(
            &helper_info,
            &[
                ("CFBundleDisplayName", "OpenLogi Agent Dev"),
                ("CFBundleIdentifier", "org.openlogi.agent.dev"),
                ("CFBundleName", "OpenLogi Agent Dev"),
            ],
        )?;
    }

    println!("    stamped local IDs: org.openlogi.openlogi.dev / org.openlogi.agent.dev");
    Ok(())
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

fn local_sign_app_if_available() -> Result<()> {
    if env::var("OPENLOGI_LOCAL_CODESIGN").as_deref() == Ok("0") {
        println!("==> local codesign: skipped (OPENLOGI_LOCAL_CODESIGN=0)");
        return Ok(());
    }

    if let Some(identity) = env_nonempty("OPENLOGI_SIGN_IDENTITY") {
        sign_app_with_timestamp(&identity, TimestampMode::Secure)?;
        return Ok(());
    }

    if let Some(identity) = env_nonempty("OPENLOGI_LOCAL_CODESIGN_IDENTITY") {
        sign_app_with_timestamp(&identity, TimestampMode::None)?;
        return Ok(());
    }

    if let Some(identity) = first_apple_development_identity()? {
        sign_app_with_timestamp(&identity, TimestampMode::None)?;
        return Ok(());
    }

    println!(
        "==> local codesign: skipped (no Apple Development identity found;          set OPENLOGI_LOCAL_CODESIGN_IDENTITY or OPENLOGI_SIGN_IDENTITY to sign)"
    );
    println!(
        "    warning: unsigned/ad-hoc local bundles with production bundle IDs can          make macOS Accessibility grants appear stale or missing"
    );
    Ok(())
}

fn sign_app_with_timestamp(identity: &str, timestamp: TimestampMode) -> Result<()> {
    let sh = Shell::new()?;
    let root = repo_root()?;
    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    let helper = app.join("Contents/Library/LoginItems/OpenLogiAgent.app");
    let overlay = app.join("Contents/Library/LoginItems/OpenLogiOverlay.app");
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
    root.join("crates/openlogi-gui/bundle/OpenLogi.entitlements")
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

fn quoted_identity(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is idiomatic in tests")]
mod tests {
    use super::*;

    fn app_with_binaries(binaries: &[&str]) -> tempfile::TempDir {
        let app = tempfile::tempdir().unwrap();
        for binary in binaries {
            let path = app.path().join(binary);
            fs_err::create_dir_all(path.parent().unwrap()).unwrap();
            fs_err::write(path, b"").unwrap();
        }
        app
    }

    #[test]
    fn verify_bundle_binaries_accepts_a_complete_bundle() {
        let app = app_with_binaries(&REQUIRED_BUNDLE_BINARIES);

        verify_bundle_binaries(app.path()).unwrap();
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

    #[test]
    fn verify_bundle_binaries_names_each_missing_binary() {
        for missing in REQUIRED_BUNDLE_BINARIES {
            let shipped: Vec<&str> = REQUIRED_BUNDLE_BINARIES
                .into_iter()
                .filter(|binary| *binary != missing)
                .collect();
            let app = app_with_binaries(&shipped);

            let error = verify_bundle_binaries(app.path()).unwrap_err();

            assert!(
                error.to_string().ends_with(missing),
                "error should name {missing}, got: {error}"
            );
        }
    }
}

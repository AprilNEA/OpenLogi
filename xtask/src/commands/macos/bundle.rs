use std::env;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context as _, Result};
use icns::{IconFamily, IconType, Image as IcnsImage, PixelFormat};
use image::imageops::FilterType;
use plist::Value;
use xshell::{Shell, cmd};

use crate::support::fs::{command_exists, ensure_dir, ensure_file, repo_root};

pub(crate) fn generate_icns() -> Result<()> {
    let root = repo_root()?;
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
    write_icns(&master, &output)?;
    println!("wrote {}", output.display());
    Ok(())
}

fn write_icns(master: &Path, output: &Path) -> Result<()> {
    let master = image::open(master)
        .with_context(|| format!("could not read app icon master {}", master.display()))?;
    let mut family = IconFamily::new();
    for (size, icon_type) in [
        (16, IconType::RGBA32_16x16),
        (32, IconType::RGBA32_16x16_2x),
        (32, IconType::RGBA32_32x32),
        (64, IconType::RGBA32_32x32_2x),
        (128, IconType::RGBA32_128x128),
        (256, IconType::RGBA32_128x128_2x),
        (256, IconType::RGBA32_256x256),
        (512, IconType::RGBA32_256x256_2x),
        (512, IconType::RGBA32_512x512),
        (1024, IconType::RGBA32_512x512_2x),
    ] {
        let rgba = master
            .resize_exact(size, size, FilterType::Lanczos3)
            .to_rgba8();
        let icon = IcnsImage::from_data(PixelFormat::RGBA, size, size, rgba.into_raw())?;
        family.add_icon_with_type(&icon, icon_type)?;
    }
    let file = fs_err::File::create(output)
        .with_context(|| format!("could not create app icon {}", output.display()))?;
    family.write(BufWriter::new(file))?;
    Ok(())
}

pub(crate) fn run() -> Result<()> {
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

    let app = root.join("target/release/bundle/osx/OpenLogi.app");
    ensure_dir(&app)?;
    embed_agent_helper(&root, &app, &xcode_env)?;
    embed_cli(&root, &app, &xcode_env)?;
    verify_bundle_binaries(&app)?;
    println!();
    println!("Bundle ready: {}", app.display());
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
const REQUIRED_BUNDLE_BINARIES: [&str; 3] = [
    "Contents/MacOS/openlogi",
    "Contents/MacOS/openlogi-gui",
    "Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
];

fn verify_bundle_binaries(app: &Path) -> Result<()> {
    for binary in REQUIRED_BUNDLE_BINARIES {
        let path = app.join(binary);
        ensure_file(&path)
            .with_context(|| format!("missing required bundle binary {}", path.display()))?;
    }
    Ok(())
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

pub(crate) fn sign_app(identity: &str) -> Result<()> {
    let sh = Shell::new()?;
    let app = repo_root()?.join("target/release/bundle/osx/OpenLogi.app");
    let helper = app.join("Contents/Library/LoginItems/OpenLogiAgent.app");
    println!("==> codesign ({identity})");
    // Inside-out signing: seal the nested helper with its own signature first,
    // then the outer app (which seals the already-signed helper). `--deep` is
    // deprecated and can't give the helper an independent signature — but a
    // stable, separately-signed helper identity is exactly what lets the agent's
    // Accessibility (TCC) grant persist across updates. So sign each explicitly.
    if helper.exists() {
        codesign_runtime(identity, &helper)?;
    }
    // The embedded CLI is a second Mach-O under Contents/MacOS; sign it with the
    // hardened runtime before the outer app so it carries a Developer ID
    // signature (its as-built ad-hoc signature would fail notarization).
    let cli = app.join("Contents/MacOS/openlogi");
    if cli.exists() {
        codesign_runtime(identity, &cli)?;
    }
    codesign_runtime(identity, &app)?;
    cmd!(sh, "codesign --verify --strict {app}").run()?;
    if helper.exists() {
        cmd!(sh, "codesign --verify --strict {helper}").run()?;
    }
    if cli.exists() {
        cmd!(sh, "codesign --verify --strict {cli}").run()?;
    }
    Ok(())
}

/// Sign one bundle with the hardened runtime + a secure timestamp.
fn codesign_runtime(identity: &str, target: &Path) -> Result<()> {
    let sh = Shell::new()?;
    cmd!(
        sh,
        "codesign --force --options runtime --timestamp --sign {identity} {target}"
    )
    .run()?;
    Ok(())
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

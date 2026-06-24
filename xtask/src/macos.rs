use std::env;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;
use icns::{IconFamily, IconType, Image as IcnsImage, PixelFormat};
use image::imageops::FilterType;
use xshell::{Shell, cmd};

use crate::util::{absolutize, command_exists, ensure_command, ensure_dir, ensure_file, repo_root};

#[derive(Parser)]
pub(crate) struct DmgMacos {
    /// App bundle to package.
    #[arg(long, default_value = "target/release/bundle/osx/OpenLogi.app")]
    app: PathBuf,
    /// Output DMG path.
    #[arg(long, default_value = "target/release/OpenLogi.dmg")]
    output: PathBuf,
    /// Developer ID identity used to sign the DMG, and the app when packaging.
    #[arg(long, env = "OPENLOGI_SIGN_IDENTITY")]
    sign_identity: Option<String>,
    /// Branded DMG background URL.
    #[arg(
        long,
        env = "OPENLOGI_DMG_BACKGROUND_URL",
        default_value = "https://assets.openlogi.org/dmg/dmg-background.tiff"
    )]
    background_url: String,
}

pub(crate) fn package_macos(args: &DmgMacos) -> Result<()> {
    bundle_macos()?;
    if let Some(identity) = &args.sign_identity {
        sign_app(identity)?;
    } else {
        println!("==> codesign: skipped (unsigned — set OPENLOGI_SIGN_IDENTITY to sign)");
    }
    dmg_macos(args)
}

pub(crate) fn generate_macos_icns() -> Result<()> {
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

pub(crate) fn bundle_macos() -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);
    let xcode_env = xcode_env()?;

    println!("==> app icon");
    generate_macos_icns()?;

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
    // lives under "OpenLogi Agent". `bundle_macos` runs `generate_macos_icns`
    // first, so the icns is already on disk. Matches the Info.plist
    // CFBundleIconFile = "AppIcon".
    let icon_src = root.join("crates/openlogi-gui/icon/AppIcon.icns");
    ensure_file(&icon_src)?;
    let resources = helper.join("Contents/Resources");
    fs_err::create_dir_all(&resources)
        .with_context(|| format!("could not create {}", resources.display()))?;
    fs_err::copy(&icon_src, resources.join("AppIcon.icns"))
        .with_context(|| "could not copy the app icon into the helper bundle".to_string())?;
    // The template ships the 0.0.0 sentinel; stamp the workspace version
    // (= xtask's own, inherited) over it so Finder and update scanners see the
    // real one.
    let version = env!("CARGO_PKG_VERSION");
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        cmd!(
            sh,
            "/usr/bin/plutil -replace {key} -string {version} {info_dst}"
        )
        .run()?;
    }

    println!("    embedded {}", helper.display());
    Ok(())
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

pub(crate) fn dmg_macos(args: &DmgMacos) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);
    let app = absolutize(&root, &args.app);
    let output = absolutize(&root, &args.output);
    ensure_dir(&app)?;
    ensure_command("create-dmg")?;

    println!("==> dmg background");
    let background = root.join("target/release/dmg-background.tiff");
    if let Some(parent) = background.parent() {
        fs_err::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let background_url = &args.background_url;
    cmd!(sh, "curl -fsSL {background_url} -o {background}")
        .run()
        .with_context(|| {
            format!(
                "failed to fetch DMG background from {}",
                args.background_url
            )
        })?;

    println!("==> dmg");
    if output.exists() {
        fs_err::remove_file(&output)
            .with_context(|| format!("could not remove {}", output.display()))?;
    }

    // Geometry is locked to the painted 760×480 background. `create-dmg` uses
    // outer window dimensions, so add the 32pt Finder title bar and keep icon
    // coordinates relative to the 760×480 content area.
    // ULMO (LZMA) compresses ~20% smaller than the default UDZO (zlib) and
    // mounts on macOS 10.15+, well under the bundle's 13.0 floor.
    cmd!(
        sh,
        "create-dmg --format ULMO --volname OpenLogi --background {background} --window-pos 240 120 --window-size 760 512 --icon-size 128 --icon OpenLogi.app 212 250 --app-drop-link 548 250 --hide-extension OpenLogi.app {output} {app}"
    )
    .run()?;

    if let Some(identity) = &args.sign_identity {
        sign_dmg(identity, &output)?;
    }

    println!();
    println!("done → {}", output.display());
    Ok(())
}

fn sign_app(identity: &str) -> Result<()> {
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
    codesign_runtime(identity, &app)?;
    cmd!(sh, "codesign --verify --strict {app}").run()?;
    if helper.exists() {
        cmd!(sh, "codesign --verify --strict {helper}").run()?;
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

fn sign_dmg(identity: &str, dmg: &Path) -> Result<()> {
    let sh = Shell::new()?;
    println!("==> codesign dmg ({identity})");
    cmd!(sh, "codesign --force --timestamp --sign {identity} {dmg}").run()?;
    cmd!(sh, "codesign --verify --verbose=2 {dmg}").run()?;
    Ok(())
}

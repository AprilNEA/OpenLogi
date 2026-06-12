use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context as _, Result};
use clap::Parser;

use crate::util::{
    TempDir, absolutize, command_exists, command_stdout, ensure_command, ensure_dir, ensure_file,
    repo_root, run, with_env,
};

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
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "could not create icon output directory {}",
            output_dir.display()
        )
    })?;

    let work = TempDir::new("openlogi-icns")?;
    let iconset = work.path().join("AppIcon.iconset");
    fs::create_dir_all(&iconset)
        .with_context(|| format!("could not create iconset directory {}", iconset.display()))?;

    // The squircle and opaque fill are baked into the 1024² master PNG, so each
    // iconset slot is just a sips downscale. sips and iconutil are macOS
    // built-ins — no SVG renderer (rsvg/resvg) needed.
    render_iconset(&iconset, |size, output| {
        run(ProcessCommand::new("sips")
            .arg("-z")
            .arg(size.to_string())
            .arg(size.to_string())
            .arg(&master)
            .arg("--out")
            .arg(output)
            .stdout(Stdio::null()))
    })?;

    run(ProcessCommand::new("iconutil")
        .arg("-c")
        .arg("icns")
        .arg(&iconset)
        .arg("-o")
        .arg(&output))?;
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

pub(crate) fn bundle_macos() -> Result<()> {
    let root = repo_root()?;
    let xcode_env = xcode_env()?;

    println!("==> app icon");
    generate_macos_icns()?;

    if env::var("OPENLOGI_BUNDLE_ASSETS").as_deref() == Ok("1") {
        println!("==> device assets: bundling (offline build)");
        run(with_env(
            ProcessCommand::new("cargo")
                .arg("run")
                .arg("-p")
                .arg("openlogi")
                .arg("--release")
                .arg("--")
                .arg("assets")
                .arg("sync")
                .current_dir(&root),
            &xcode_env,
        ))?;
    } else {
        println!("==> device assets: on-demand (not bundled; fetched at first launch)");
        let assets = root.join("crates/openlogi-gui/assets");
        if assets.exists() {
            fs::remove_dir_all(&assets)
                .with_context(|| format!("could not remove {}", assets.display()))?;
        }
        fs::create_dir_all(&assets)
            .with_context(|| format!("could not create {}", assets.display()))?;
    }

    println!("==> bundle (.app)");
    if !command_exists("cargo-bundle") {
        let mut install = ProcessCommand::new("cargo");
        install
            .arg("install")
            .arg("cargo-bundle")
            .arg("--locked")
            .env("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", "/usr/bin/cc");
        run(with_env(&mut install, &xcode_env))?;
    }
    run(with_env(
        ProcessCommand::new("cargo")
            .arg("bundle")
            .arg("--release")
            .current_dir(root.join("crates/openlogi-gui")),
        &xcode_env,
    ))?;

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
    println!("==> agent helper (build)");
    run(with_env(
        ProcessCommand::new("cargo")
            .arg("build")
            .arg("-p")
            .arg("openlogi-agent")
            .arg("--release")
            .current_dir(root),
        xcode_env,
    ))?;
    let agent_bin = root.join("target/release/openlogi-agent");
    ensure_file(&agent_bin)?;

    let helper = app.join("Contents/Library/LoginItems/OpenLogiAgent.app");
    let helper_macos = helper.join("Contents/MacOS");
    fs::create_dir_all(&helper_macos)
        .with_context(|| format!("could not create {}", helper_macos.display()))?;
    fs::copy(&agent_bin, helper_macos.join("openlogi-agent"))
        .with_context(|| "could not copy the agent binary into the helper bundle".to_string())?;
    let info_src = root.join("crates/openlogi-agent/macos/Info.plist");
    ensure_file(&info_src)?;
    let info_dst = helper.join("Contents/Info.plist");
    fs::copy(&info_src, &info_dst)
        .with_context(|| "could not write the helper Info.plist".to_string())?;
    // The template ships the 0.0.0 dev version (the hand-bundled dev flow
    // copies it verbatim); stamp the workspace version (= xtask's own,
    // inherited) over it so Finder and update scanners see the real one.
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        run(ProcessCommand::new("/usr/bin/plutil")
            .arg("-replace")
            .arg(key)
            .arg("-string")
            .arg(env!("CARGO_PKG_VERSION"))
            .arg(&info_dst))?;
    }

    println!("    embedded {}", helper.display());
    Ok(())
}

fn xcode_env() -> Result<Vec<(String, String)>> {
    let developer_dir = env::var("OPENLOGI_DEVELOPER_DIR")
        .unwrap_or_else(|_| "/Applications/Xcode.app/Contents/Developer".to_string());
    let sdkroot = command_stdout(
        ProcessCommand::new("/usr/bin/xcrun")
            .arg("--sdk")
            .arg("macosx")
            .arg("--show-sdk-path")
            .env("DEVELOPER_DIR", &developer_dir),
    )?;
    Ok(vec![
        ("DEVELOPER_DIR".to_string(), developer_dir),
        ("SDKROOT".to_string(), sdkroot.trim().to_string()),
    ])
}

pub(crate) fn dmg_macos(args: &DmgMacos) -> Result<()> {
    let root = repo_root()?;
    let app = absolutize(&root, &args.app);
    let output = absolutize(&root, &args.output);
    ensure_dir(&app)?;
    ensure_command("create-dmg")?;

    println!("==> dmg background");
    let background = root.join("target/release/dmg-background.tiff");
    if let Some(parent) = background.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    run(ProcessCommand::new("curl")
        .arg("-fsSL")
        .arg(&args.background_url)
        .arg("-o")
        .arg(&background))
    .with_context(|| {
        format!(
            "failed to fetch DMG background from {}",
            args.background_url
        )
    })?;

    println!("==> dmg");
    if output.exists() {
        fs::remove_file(&output)
            .with_context(|| format!("could not remove {}", output.display()))?;
    }

    // Geometry is locked to the painted 760×480 background. `create-dmg` uses
    // outer window dimensions, so add the 32pt Finder title bar and keep icon
    // coordinates relative to the 760×480 content area.
    // ULMO (LZMA) compresses ~20% smaller than the default UDZO (zlib) and
    // mounts on macOS 10.15+, well under the bundle's 13.0 floor.
    run(ProcessCommand::new("create-dmg")
        .arg("--format")
        .arg("ULMO")
        .arg("--volname")
        .arg("OpenLogi")
        .arg("--background")
        .arg(&background)
        .arg("--window-pos")
        .arg("240")
        .arg("120")
        .arg("--window-size")
        .arg("760")
        .arg("512")
        .arg("--icon-size")
        .arg("128")
        .arg("--icon")
        .arg("OpenLogi.app")
        .arg("212")
        .arg("250")
        .arg("--app-drop-link")
        .arg("548")
        .arg("250")
        .arg("--hide-extension")
        .arg("OpenLogi.app")
        .arg(&output)
        .arg(&app))?;

    if let Some(identity) = &args.sign_identity {
        sign_dmg(identity, &output)?;
    }

    println!();
    println!("done → {}", output.display());
    Ok(())
}

fn sign_app(identity: &str) -> Result<()> {
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
    run(ProcessCommand::new("codesign")
        .arg("--verify")
        .arg("--strict")
        .arg(&app))?;
    if helper.exists() {
        run(ProcessCommand::new("codesign")
            .arg("--verify")
            .arg("--strict")
            .arg(&helper))?;
    }
    Ok(())
}

/// Sign one bundle with the hardened runtime + a secure timestamp.
fn codesign_runtime(identity: &str, target: &Path) -> Result<()> {
    run(ProcessCommand::new("codesign")
        .arg("--force")
        .arg("--options")
        .arg("runtime")
        .arg("--timestamp")
        .arg("--sign")
        .arg(identity)
        .arg(target))
}

fn sign_dmg(identity: &str, dmg: &Path) -> Result<()> {
    println!("==> codesign dmg ({identity})");
    run(ProcessCommand::new("codesign")
        .arg("--force")
        .arg("--timestamp")
        .arg("--sign")
        .arg(identity)
        .arg(dmg))?;
    run(ProcessCommand::new("codesign")
        .arg("--verify")
        .arg("--verbose=2")
        .arg(dmg))
}

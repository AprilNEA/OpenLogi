use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::fs::{absolutize, ensure_command, ensure_dir, repo_root};

#[derive(Parser)]
pub(crate) struct Args {
    /// App bundle to package.
    #[arg(long, default_value = "target/release/bundle/osx/OpenLogi.app")]
    pub(crate) app: PathBuf,
    /// Output DMG path.
    #[arg(long, default_value = "target/release/OpenLogi.dmg")]
    pub(crate) output: PathBuf,
    /// Developer ID identity used to sign the DMG, and the app when packaging.
    #[arg(long, env = "OPENLOGI_SIGN_IDENTITY")]
    pub(crate) sign_identity: Option<String>,
    /// Branded DMG background URL.
    #[arg(
        long,
        env = "OPENLOGI_DMG_BACKGROUND_URL",
        default_value = "https://assets.openlogi.org/dmg/dmg-background.tiff"
    )]
    pub(crate) background_url: String,
}

pub(crate) fn run(args: &Args) -> Result<()> {
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

fn sign_dmg(identity: &str, dmg: &Path) -> Result<()> {
    let sh = Shell::new()?;
    println!("==> codesign dmg ({identity})");
    cmd!(sh, "codesign --force --timestamp --sign {identity} {dmg}").run()?;
    cmd!(sh, "codesign --verify --verbose=2 {dmg}").run()?;
    Ok(())
}

//! The app icon: one committed 1024² master, rendered into an iconset and
//! encoded by Apple's own tool.

use std::path::Path;

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

use crate::support::fs::{ensure_file, repo_root};

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

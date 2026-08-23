//! The macOS pipeline: Icon Composer documents in, an app bundle's icons out.
//!
//! `actool` renders a document — layers, fill and material, versioned as JSON
//! plus its artwork — into both an `.icns` (what macOS 13 through 25 draw, and
//! what every list of our processes reads) and an asset catalog (what macOS 26
//! composes the layered icon from). Neither is a source file; both are build
//! outputs of the same document.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use strum::VariantArray as _;
use xshell::{Shell, cmd};

use super::{AppIcon, IconPipeline};
use crate::support::fs::{ensure_dir, ensure_file, repo_root};
use crate::support::info_plist::{read_plist_string, stamp_plist_strings};

/// `OpenLogi.app`'s icons: the one it wears, and the alternates it can switch
/// to at runtime.
pub(crate) struct AppBundle;

/// Where the compiled icons land. `cargo-bundle` copies `AppIcon.icns` out of
/// here (`openlogi-desktop/Cargo.toml` names it); the catalog and the
/// alternates are installed into the bundle by [`IconPipeline::install`].
const OUTPUT_DIR: &str = "crates/openlogi-desktop/icon";

/// Where the alternates live inside the bundle, so the app can hand one to
/// macOS at runtime.
const ALTERNATES_DIR: &str = "Contents/Resources/Icons";

/// The name every component's `Info.plist` gives the icon, in both spellings:
/// `CFBundleIconFile` for the `.icns`, `CFBundleIconName` for the catalog
/// entry. `actool` names its output after the document it compiled, so the
/// document is staged under this name rather than its repository one.
pub(crate) const ICON_NAME: &str = "AppIcon";

/// The compiled asset catalog, as `actool` always names it.
const CATALOG: &str = "Assets.car";

/// Deployment target handed to `actool`; mirrors `osx_minimum_system_version`
/// in `openlogi-desktop/Cargo.toml`.
const MINIMUM_MACOS: &str = "13.0";

impl IconPipeline for AppBundle {
    fn compile(&self) -> Result<()> {
        let root = repo_root()?;
        let output_dir = root.join(OUTPUT_DIR);
        fs_err::create_dir_all(&output_dir).with_context(|| {
            format!(
                "could not create icon output directory {}",
                output_dir.display()
            )
        })?;
        for &icon in AppIcon::VARIANTS {
            compile_document(&root, &output_dir, icon)?;
        }
        Ok(())
    }

    /// Put everything past the `.icns` into `app`: the catalog macOS 26
    /// composes the layered icon from, and the alternates the app hands to
    /// macOS when a user picks one.
    ///
    /// Only the app carries them. It is the only bundle whose icon comes from a
    /// catalog, while the nested helpers show up in lists — Login Items, the
    /// privacy panes — that read the `.icns` they already ship. `cargo-bundle`
    /// writes that `.icns` and the `CFBundleIconFile` naming it; the rest is
    /// ours to add, and all of it has to land before signing seals the bundle.
    fn install(&self, app: &Path) -> Result<()> {
        let compiled = repo_root()?.join(OUTPUT_DIR);
        let resources = app.join("Contents/Resources");
        fs_err::create_dir_all(&resources)
            .with_context(|| format!("could not create {}", resources.display()))?;
        let catalog = compiled.join(CATALOG);
        ensure_file(&catalog)?;
        fs_err::copy(&catalog, resources.join(CATALOG))
            .with_context(|| format!("could not copy {CATALOG} into the bundle"))?;

        for &icon in AppIcon::VARIANTS {
            let Some(target) = alternate(app, icon) else {
                continue;
            };
            let source = compiled.join(format!("{}.icns", compiled_stem(icon)));
            ensure_file(&source)?;
            fs_err::create_dir_all(app.join(ALTERNATES_DIR))
                .with_context(|| format!("could not create {ALTERNATES_DIR} in the bundle"))?;
            fs_err::copy(&source, &target)
                .with_context(|| format!("could not copy the {icon} icon into the bundle"))?;
        }

        stamp_plist_strings(
            &app.join("Contents/Info.plist"),
            &[("CFBundleIconName", ICON_NAME)],
        )
    }

    fn verify(&self, app: &Path) -> Result<()> {
        let catalog = app.join("Contents/Resources").join(CATALOG);
        if !catalog.is_file() {
            bail!(
                "app: missing the icon asset catalog at {}",
                catalog.display()
            );
        }
        for &icon in AppIcon::VARIANTS {
            let Some(path) = alternate(app, icon) else {
                continue;
            };
            if !path.is_file() {
                bail!("app: missing the {icon} icon at {}", path.display());
            }
        }
        let plist = app.join("Contents/Info.plist");
        let declared = read_plist_string(&plist, "CFBundleIconName")?;
        if declared.as_deref() != Some(ICON_NAME) {
            bail!(
                "app: CFBundleIconName is {declared:?}, expected {ICON_NAME:?} ({})",
                plist.display()
            );
        }
        Ok(())
    }
}

/// This icon's Icon Composer document, relative to the repository root.
fn document(icon: AppIcon) -> &'static str {
    match icon {
        AppIcon::Openlogi => "design/icon/openlogi.icon",
        AppIcon::Midnight => "design/icon/openlogi-midnight.icon",
    }
}

/// What this icon's compiled `.icns` is called: the default fills the bundle's
/// own icon slot, the alternates are named after themselves.
fn compiled_stem(icon: AppIcon) -> String {
    if icon.is_default() {
        ICON_NAME.to_owned()
    } else {
        icon.to_string()
    }
}

/// Where `icon` ships inside `app` — `None` for the default, which *is* the
/// bundle's icon and needs no second copy.
pub(crate) fn alternate(app: &Path, icon: AppIcon) -> Option<PathBuf> {
    (!icon.is_default()).then(|| app.join(ALTERNATES_DIR).join(format!("{icon}.icns")))
}

/// Compile one document. The default keeps its catalog — the alternates are
/// only ever handed to macOS as an image, which is what the `.icns` is.
fn compile_document(root: &Path, output_dir: &Path, icon: AppIcon) -> Result<()> {
    let sh = Shell::new()?;
    let source = root.join(document(icon));
    // An Icon Composer document is a package: a directory holding `icon.json`
    // and the artwork it names.
    ensure_dir(&source)?;

    let work = tempfile::Builder::new()
        .prefix("openlogi-app-icon-")
        .tempdir()
        .context("could not create temporary icon directory")?;
    // actool names every output after the document it compiled, so the name the
    // bundle wants is the name the document is staged under.
    let stem = compiled_stem(icon);
    let staged = work.path().join(format!("{stem}.icon"));
    cmd!(sh, "/usr/bin/ditto {source} {staged}")
        .run()
        .context("could not stage the icon document")?;
    // actool writes into an existing directory only.
    let compiled = work.path().join("compiled");
    fs_err::create_dir_all(&compiled)
        .with_context(|| format!("could not create {}", compiled.display()))?;
    let partial_plist = work.path().join("icon.plist");

    // `actool` reports everything — errors included — as a plist on stdout and
    // nothing on stderr, so its output is only worth showing when it fails.
    let run = cmd!(
        sh,
        "/usr/bin/xcrun actool {staged}
         --compile {compiled}
         --platform macosx
         --minimum-deployment-target {MINIMUM_MACOS}
         --target-device mac
         --app-icon {stem}
         --output-partial-info-plist {partial_plist}"
    )
    .ignore_status()
    .output()
    .context("could not run actool (it ships with Xcode, not the command line tools)")?;
    if !run.status.success() {
        bail!(
            "actool could not compile {}:\n{}",
            source.display(),
            String::from_utf8_lossy(&run.stdout)
        );
    }

    // actool always calls the catalog `Assets.car`, so the icons are compiled
    // apart and only what each one contributes is kept.
    let icns = format!("{stem}.icns");
    take(&compiled.join(&icns), &output_dir.join(&icns))?;
    if icon.is_default() {
        take(&compiled.join(CATALOG), &output_dir.join(CATALOG))?;
    }
    println!("compiled {icon} from {}", document(icon));
    Ok(())
}

/// Move one compiled output into place, replacing whatever was there.
fn take(from: &Path, to: &Path) -> Result<()> {
    ensure_file(from)?;
    fs_err::rename(from, to)
        .or_else(|_| fs_err::copy(from, to).map(|_| ()))
        .with_context(|| format!("could not write {}", to.display()))
}

#[cfg(test)]
mod tests;

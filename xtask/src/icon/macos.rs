//! The macOS pipeline: Icon Composer documents in, an app bundle's icons out.
//!
//! `actool` renders a document — layers, fill and material, versioned as JSON
//! plus its artwork — into both an `.icns` (what macOS 13 through 25 draw, and
//! what every list of our processes reads) and an asset catalog (what macOS 26
//! composes the layered icon from). Neither is a source file; both are build
//! outputs of the same document.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use xshell::{Shell, cmd};

use super::{AppIcon, IconPipeline};
use crate::support::fs::{ensure_dir, ensure_file, repo_root};
use crate::support::info_plist::{read_plist_string, stamp_plist_strings};
use crate::support::xcode;

/// `OpenLogi.app`'s icons: the one it wears, and the alternates it can switch
/// to at runtime.
pub(crate) struct AppBundle;

/// Where the compiled icons land. `cargo-bundle` copies `AppIcon.icns` out of
/// here (`openlogi-desktop/Cargo.toml` names it); the catalog and the
/// alternates are installed into the bundle by [`IconPipeline::install`].
const OUTPUT_DIR: &str = "crates/openlogi-desktop/icon";

/// Where the icons Settings offers live inside the bundle: a preview for each,
/// and the `.icns` of every alternate for the app to hand to macOS at runtime.
const ICONS_DIR: &str = "Contents/Resources/Icons";

/// Where the previews are rendered to before the bundle is assembled.
const PREVIEWS_DIR: &str = "previews";

/// Preview edge length, in pixels: twice the point size Settings draws its icon
/// cards at, so the picker stays crisp on a Retina display.
const PREVIEW_PIXELS: &str = "128";

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
        for icon in AppIcon::ALL {
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

        // Both bundle directories are assembled in place and reused, so a run
        // of the app that applied an alternate leaves its custom icon behind —
        // and `codesign` refuses a bundle carrying one ("resource fork, Finder
        // information, or similar detritus"). A freshly built bundle wears what
        // it was compiled with.
        if appicon::has_custom_icon(app) {
            appicon::reset_file(app).context("could not clear the bundle's custom icon")?;
        }

        // Rebuilt rather than added to, so an icon that was renamed or dropped
        // cannot linger in a bundle that keeps being reused.
        let icons = app.join(ICONS_DIR);
        if icons.exists() {
            fs_err::remove_dir_all(&icons)
                .with_context(|| format!("could not clear {}", icons.display()))?;
        }
        fs_err::create_dir_all(&icons)
            .with_context(|| format!("could not create {ICONS_DIR} in the bundle"))?;
        for icon in AppIcon::ALL {
            // Every icon Settings offers needs a preview; only the alternates
            // need an `.icns`, since the default is the bundle's own icon.
            put(
                &compiled.join(PREVIEWS_DIR).join(preview_file(icon)),
                &preview(app, icon),
            )?;
            if let Some(target) = alternate(app, icon) {
                put(
                    &compiled.join(format!("{}.icns", compiled_stem(icon))),
                    &target,
                )?;
            }
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
        for icon in AppIcon::ALL {
            let preview = preview(app, icon);
            if !preview.is_file() {
                bail!(
                    "app: missing the {icon} icon preview at {}",
                    preview.display()
                );
            }
            if let Some(path) = alternate(app, icon)
                && !path.is_file()
            {
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
        AppIcon::Prism => "design/icon/openlogi-prism.icon",
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

/// Where `icon`'s `.icns` ships inside `app` — `None` for the default, which
/// *is* the bundle's icon and needs no second copy.
pub(crate) fn alternate(app: &Path, icon: AppIcon) -> Option<PathBuf> {
    (!icon.is_default()).then(|| app.join(ICONS_DIR).join(format!("{icon}.icns")))
}

/// Where `icon`'s preview ships inside `app`. Settings draws these, so every
/// icon has one — the default included.
pub(crate) fn preview(app: &Path, icon: AppIcon) -> PathBuf {
    app.join(ICONS_DIR).join(preview_file(icon))
}

/// A preview's file name, the same in the build directory and in the bundle.
fn preview_file(icon: AppIcon) -> String {
    format!("{icon}.png")
}

/// Copy one compiled file into the bundle.
fn put(from: &Path, to: &Path) -> Result<()> {
    ensure_file(from)?;
    fs_err::copy(from, to)
        .map(|_| ())
        .with_context(|| format!("could not copy {} into the bundle", from.display()))
}

/// Compile one document. The default keeps its catalog — the alternates are
/// only ever handed to macOS as an image, which is what the `.icns` is.
fn compile_document(root: &Path, output_dir: &Path, icon: AppIcon) -> Result<()> {
    let sh = Shell::new()?;
    let source = root.join(document(icon));
    // An Icon Composer document is a package: a directory holding `icon.json`
    // and the artwork it names.
    ensure_dir(&source)?;

    // `actool` costs seconds and every `cargo run` of the GUI comes through
    // here, so a document that has not changed since it was last compiled is
    // skipped — by age, not by presence: an edited layer must not leave the dev
    // bundle testing an icon the release build would never produce.
    let outputs = compiled_outputs(output_dir, icon);
    if outputs_are_current(&source, &outputs)? {
        println!("{icon} is up to date");
        return Ok(());
    }

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

    // Under the build's own Xcode, not whichever one the machine happens to
    // have selected: a runner can carry several, and an Icon Composer document
    // needs 26 or newer.
    //
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
    .envs(xcode::env()?.iter().map(|(key, value)| (key, value)))
    .ignore_status()
    .output()
    .context("could not run actool (it ships with Xcode, not the command line tools)")?;
    if !run.status.success() {
        bail!(
            "actool could not compile {}: an Icon Composer document needs Xcode 26 \
             or newer, and OPENLOGI_DEVELOPER_DIR picks which Xcode is used.\n{}",
            source.display(),
            String::from_utf8_lossy(&run.stdout)
        );
    }

    // actool always calls the catalog `Assets.car`, so the icons are compiled
    // apart and only what each one contributes is kept.
    let icns = format!("{stem}.icns");
    let installed_icns = output_dir.join(&icns);
    take(&compiled.join(&icns), &installed_icns)?;
    if icon.is_default() {
        take(&compiled.join(CATALOG), &output_dir.join(CATALOG))?;
    }

    // The picker in Settings draws a render of the compiled icon rather than
    // the document's artwork, so what a user picks from is what macOS will
    // actually draw — fill, material and all.
    let previews = output_dir.join(PREVIEWS_DIR);
    fs_err::create_dir_all(&previews)
        .with_context(|| format!("could not create {}", previews.display()))?;
    let preview = previews.join(preview_file(icon));
    cmd!(
        sh,
        "/usr/bin/sips -s format png -z {PREVIEW_PIXELS} {PREVIEW_PIXELS} {installed_icns} --out {preview}"
    )
    .ignore_stdout()
    .run()
    .context("could not render the icon preview")?;

    println!("compiled {icon} from {}", document(icon));
    Ok(())
}

/// Everything compiling `icon` writes.
fn compiled_outputs(output_dir: &Path, icon: AppIcon) -> Vec<PathBuf> {
    let mut outputs = vec![
        output_dir.join(format!("{}.icns", compiled_stem(icon))),
        output_dir.join(PREVIEWS_DIR).join(preview_file(icon)),
    ];
    if icon.is_default() {
        outputs.push(output_dir.join(CATALOG));
    }
    outputs
}

/// Whether every output is present and newer than everything in `source`.
fn outputs_are_current(source: &Path, outputs: &[PathBuf]) -> Result<bool> {
    let source_touched = newest_change(source)?;
    for output in outputs {
        let Ok(metadata) = fs_err::metadata(output) else {
            return Ok(false);
        };
        if metadata.modified()? <= source_touched {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The most recent modification anywhere under `path`. A document is a
/// directory, and editing a layer inside it leaves the directory's own
/// timestamp untouched.
fn newest_change(path: &Path) -> Result<SystemTime> {
    let metadata = fs_err::metadata(path)?;
    let mut newest = metadata.modified()?;
    if metadata.is_dir() {
        for entry in fs_err::read_dir(path)? {
            newest = newest.max(newest_change(&entry?.path())?);
        }
    }
    Ok(newest)
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

//! The app icon: one Icon Composer document, compiled by Apple's own tool into
//! the two shapes a bundle needs.
//!
//! `actool` renders `design/icon/openlogi.icon` — layers, fill and material,
//! versioned as JSON plus its artwork — into both an `.icns` (what macOS 13
//! through 25 draw, and what every list of our processes reads) and an asset
//! catalog (what macOS 26 composes the layered icon from). Neither is a source
//! file; both are build outputs of the same document.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use strum::{Display, VariantArray};
use xshell::{Shell, cmd};

use super::info_plist::stamp_plist_strings;
use crate::support::fs::{ensure_dir, ensure_file, repo_root};

/// An icon the app can wear, one Icon Composer document each.
///
/// The set lives here because every pass over it — compiling the documents,
/// putting the alternates where the app can find them, checking they arrived —
/// has to cover a newly added icon without anyone remembering to extend a list.
/// `Display` renders the name the icon is known by outside the build: the file
/// it ships as, and the value the app persists once a user picks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, VariantArray)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum AppIcon {
    /// What the app wears out of the box: the icon `CFBundleIconFile` and
    /// `CFBundleIconName` name, and the only one macOS draws on its own.
    Openlogi,
    /// The dark alternate.
    Midnight,
}

impl AppIcon {
    /// The icon a bundle wears until something switches it.
    const DEFAULT: Self = Self::Openlogi;

    /// This icon's Icon Composer document, relative to the repository root.
    fn document(self) -> &'static str {
        match self {
            Self::Openlogi => "design/icon/openlogi.icon",
            Self::Midnight => "design/icon/openlogi-midnight.icon",
        }
    }

    /// What this icon's compiled `.icns` is called: the default fills the
    /// bundle's own icon slot, the alternates are named after themselves.
    fn compiled_stem(self) -> String {
        if self == Self::DEFAULT {
            ICON_NAME.to_owned()
        } else {
            self.to_string()
        }
    }
}

/// Where the compiled icons land. `cargo-bundle` copies `AppIcon.icns` out of
/// here (`openlogi-desktop/Cargo.toml` names it); the catalog and the
/// alternates are installed by [`install_app_icon`].
const OUTPUT_DIR: &str = "crates/openlogi-desktop/icon";

/// Where the alternates live inside the bundle, so the app can hand one to
/// macOS at runtime.
const ALTERNATES_DIR: &str = "Contents/Resources/Icons";

/// The name every component's `Info.plist` gives the icon, in both spellings:
/// `CFBundleIconFile` for the `.icns`, `CFBundleIconName` for the catalog
/// entry. `actool` names its output after the document it compiled, so the
/// document is staged under this name rather than its repository one.
pub(super) const ICON_NAME: &str = "AppIcon";

/// The compiled asset catalog, as `actool` always names it.
pub(super) const CATALOG: &str = "Assets.car";

/// Deployment target handed to `actool`; mirrors `osx_minimum_system_version`
/// in `openlogi-desktop/Cargo.toml`.
const MINIMUM_MACOS: &str = "13.0";

/// Compile every icon into `crates/openlogi-desktop/icon`.
pub(crate) fn generate_app_icon() -> Result<()> {
    let root = repo_root()?;
    let output_dir = root.join(OUTPUT_DIR);
    fs_err::create_dir_all(&output_dir).with_context(|| {
        format!(
            "could not create icon output directory {}",
            output_dir.display()
        )
    })?;
    for &icon in AppIcon::VARIANTS {
        compile(&root, &output_dir, icon)?;
    }
    Ok(())
}

/// Compile one document. The default keeps its catalog — the alternates are
/// only ever handed to macOS as an image, which is what the `.icns` is.
fn compile(root: &Path, output_dir: &Path, icon: AppIcon) -> Result<()> {
    let sh = Shell::new()?;
    let source = root.join(icon.document());
    // An Icon Composer document is a package: a directory holding `icon.json`
    // and the artwork it names.
    ensure_dir(&source)?;

    let work = tempfile::Builder::new()
        .prefix("openlogi-app-icon-")
        .tempdir()
        .context("could not create temporary icon directory")?;
    // actool names every output after the document it compiled, so the name the
    // bundle wants is the name the document is staged under.
    let stem = icon.compiled_stem();
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
    let compile = cmd!(
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
    if !compile.status.success() {
        bail!(
            "actool could not compile {}:\n{}",
            source.display(),
            String::from_utf8_lossy(&compile.stdout)
        );
    }

    // actool always calls the catalog `Assets.car`, so the icons are compiled
    // apart and only what each one contributes is kept.
    let icns = format!("{stem}.icns");
    take(&compiled.join(&icns), &output_dir.join(&icns))?;
    if icon == AppIcon::DEFAULT {
        take(&compiled.join(CATALOG), &output_dir.join(CATALOG))?;
    }
    println!("compiled {icon} from {}", icon.document());
    Ok(())
}

/// Move one compiled output into place, replacing whatever was there.
fn take(from: &Path, to: &Path) -> Result<()> {
    ensure_file(from)?;
    fs_err::rename(from, to)
        .or_else(|_| fs_err::copy(from, to).map(|_| ()))
        .with_context(|| format!("could not write {}", to.display()))
}

/// Put everything past the `.icns` into `app`: the catalog macOS 26 composes
/// the layered icon from, and the alternates the app hands to macOS when a user
/// picks one.
///
/// Only the app carries them. It is the only bundle whose icon comes from a
/// catalog, while the nested helpers show up in lists — Login Items, the
/// privacy panes — that read the `.icns` they already ship. `cargo-bundle`
/// writes that `.icns` and the `CFBundleIconFile` naming it; the rest is ours to
/// add, and all of it has to land before signing seals the bundle.
pub(crate) fn install_app_icon(app: &Path) -> Result<()> {
    let root = repo_root()?;
    let compiled = root.join(OUTPUT_DIR);
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
        let source = compiled.join(format!("{}.icns", icon.compiled_stem()));
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

/// Where `icon` ships inside `app` — `None` for the default, which *is* the
/// bundle's icon: returning to it clears the override instead of applying a
/// file, so it needs no second copy.
pub(super) fn alternate(app: &Path, icon: AppIcon) -> Option<PathBuf> {
    (icon != AppIcon::DEFAULT).then(|| app.join(ALTERNATES_DIR).join(format!("{icon}.icns")))
}

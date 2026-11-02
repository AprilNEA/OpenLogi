use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2_hasher::Sha2Hasher;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

// The manifest advertises the identity the shipped bundle is stamped with, so
// both come from one constant.
use openlogi_core::brand::APP_ID;
use openlogi_core::update_feed::MANIFEST_FILE_NAME;

use crate::support::release_tag::ReleaseTag;
use crate::support::update_feed::{self, BASE_URL_ENV, MACOS_MINIMUM_OS_VERSION};

/// Windows 10+. Informational — the client updater doesn't gate on it today,
/// and everything that can run OpenLogi reports at least 10.0.
const WINDOWS_MINIMUM_OS_VERSION: &str = "10.0";

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory containing release artifacts.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Root of the tree to upload to the update host. The manifest goes into
    /// the release's feed record directory under it. Keep it out of
    /// `--dist`, whose contents are uploaded as release assets.
    #[arg(long, default_value = ".release/stage")]
    stage: PathBuf,
    /// Release tag, for example `v0.2.0`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = BASE_URL_ENV)]
    base_url: String,
    /// Also emit the per-arch Windows `.msi`/`.zip` entries. Off by default so
    /// the manifest can never reference objects the release workflow's R2
    /// upload step doesn't ship. The release workflow passes it and ships the
    /// zip/msi to the `releases/` prefix; the Windows client downloads the
    /// `.msi` and installs it via gpui-updater's staged msiexec flow.
    #[arg(long)]
    include_windows: bool,
}

/// The built-in update engine's manifest. A release's feed record holds the
/// one for that release alone; `release channels` serves it from a channel
/// directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    schema_version: u8,
    app_id: String,
    pub(super) version: String,
    pub(super) tag: String,
    /// The channel the file is served on. A release's own record names the
    /// release's channel; `release channels` renames it to the directory's.
    pub(super) channel: String,
    published_at: String,
    release_url: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    name: String,
    url: String,
    signature_url: String,
    os: String,
    arch: String,
    format: String,
    content_type: String,
    size: u64,
    sha256: String,
    minimum_os_version: String,
}

/// The per-OS constants of an updater-relevant artifact, derived from its file
/// name. The Linux packages (`.deb`/`.rpm`) are deliberately absent: those
/// installs update through the distro package manager, not the in-app updater.
struct Classified {
    os: &'static str,
    arch: String,
    format: &'static str,
    content_type: &'static str,
    minimum_os_version: &'static str,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let tag = ReleaseTag::parse(&args.tag)?;
    let release_base = update_feed::release_url(&args.base_url, &tag);
    let assets = collect_assets(&args.dist, &release_base, args.include_windows)?;
    // The DMGs are the publish gate's guaranteed artifact set; the Windows
    // legs are best-effort per arch (a failed leg publishes without them), so
    // their absence must not sink the whole manifest.
    if !assets.iter().any(|asset| asset.os == "macos") {
        bail!("no architecture-specific DMG assets found for manifest");
    }

    let manifest = Manifest {
        schema_version: 1,
        app_id: APP_ID.to_owned(),
        version: tag.version().to_string(),
        tag: tag.to_string(),
        channel: tag.channel().as_str().to_owned(),
        published_at: OffsetDateTime::from(SystemTime::now())
            .format(&Rfc3339)
            .context("could not format current timestamp")?,
        release_url: update_feed::release_page_url(&tag),
        assets,
    };

    let output = args
        .stage
        .join(update_feed::record_dir(&tag))
        .join(MANIFEST_FILE_NAME);
    manifest.write(&output)?;
    println!("{}", output.display());
    Ok(())
}

impl Manifest {
    /// The manifest as the update host serves it.
    pub(super) fn render(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// Write [`Self::render`] to `path`, creating its directory.
    pub(super) fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs_err::create_dir_all(parent).with_context(|| {
                format!("could not create manifest directory {}", parent.display())
            })?;
        }
        fs_err::write(path, self.render()?)
            .with_context(|| format!("could not write manifest to {}", path.display()))
    }
}

fn collect_assets(dist: &Path, release_base: &str, include_windows: bool) -> Result<Vec<Asset>> {
    let mut assets = Vec::new();
    for entry in fs_err::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(classified) = classify(name) else {
            continue;
        };
        // Gated so the manifest and the R2 upload step can never disagree
        // about the Windows artifacts — see the `include_windows` arg doc.
        if classified.os == "windows" && !include_windows {
            continue;
        }
        let signature_name = format!("{name}.minisig");
        let signature_path = dist.join(&signature_name);
        if !signature_path.is_file() {
            bail!(
                "missing minisign signature {} for updater artifact {}",
                signature_path.display(),
                path.display()
            );
        }
        assets.push(Asset {
            name: name.to_string(),
            url: format!("{release_base}/{name}"),
            signature_url: format!("{release_base}/{signature_name}"),
            os: classified.os.to_owned(),
            arch: classified.arch,
            format: classified.format.to_owned(),
            content_type: classified.content_type.to_owned(),
            size: path
                .metadata()
                .with_context(|| format!("could not stat {}", path.display()))?
                .len(),
            sha256: path
                .sha256()
                .with_context(|| format!("could not hash artifact {}", path.display()))?,
            minimum_os_version: classified.minimum_os_version.to_owned(),
        });
    }
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(assets)
}

/// Map an artifact file name onto its manifest constants; `None` for anything
/// the updater can't consume (SHA256SUMS, the Linux packages, the minisigs
/// themselves).
fn classify(name: &str) -> Option<Classified> {
    if let Some(stem) = name.strip_suffix(".dmg") {
        return Some(Classified {
            os: "macos",
            arch: platform_arch(stem, "-macos-")?,
            format: "dmg",
            content_type: "application/x-apple-diskimage",
            minimum_os_version: MACOS_MINIMUM_OS_VERSION,
        });
    }
    if let Some(stem) = name.strip_suffix(".msi") {
        return Some(Classified {
            os: "windows",
            arch: platform_arch(stem, "-windows-")?,
            format: "msi",
            content_type: "application/x-msi",
            minimum_os_version: WINDOWS_MINIMUM_OS_VERSION,
        });
    }
    if let Some(stem) = name.strip_suffix(".zip") {
        return Some(Classified {
            os: "windows",
            arch: platform_arch(stem, "-windows-")?,
            format: "zip",
            content_type: "application/zip",
            minimum_os_version: WINDOWS_MINIMUM_OS_VERSION,
        });
    }
    None
}

/// The `arm64`/`x86_64` suffix after the `-<os>-` marker, or `None` when the
/// stem doesn't carry one (which also filters out non-artifact archives).
fn platform_arch(stem: &str, marker: &str) -> Option<String> {
    let (_, arch) = stem.rsplit_once(marker)?;
    matches!(arch, "arm64" | "x86_64").then(|| arch.to_string())
}

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serde::Serialize;
use sha2_hasher::Sha2Hasher;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const APP_ID: &str = "org.openlogi.openlogi";
const CHANNEL: &str = "stable";
const MACOS_MINIMUM_OS_VERSION: &str = "13.0";
/// Windows 10+. Informational — the client updater doesn't gate on it today,
/// and everything that can run OpenLogi reports at least 10.0.
const WINDOWS_MINIMUM_OS_VERSION: &str = "10.0";

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory containing release artifacts.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Output manifest path.
    #[arg(long, default_value = "dist/latest.json")]
    output: PathBuf,
    /// Release tag, for example `v0.2.0`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = "OPENLOGI_UPDATE_BASE_URL")]
    base_url: String,
    /// Also emit the per-arch Windows `.msi`/`.zip` entries. Off by default so
    /// the manifest can never reference objects the release workflow's R2
    /// upload step doesn't ship. The release workflow passes it and ships the
    /// zip/msi to the `releases/` prefix; the entries are notify-only (the
    /// Windows client resolves a check but installs from the GitHub Release —
    /// `INSTALL_SUPPORTED` is false).
    #[arg(long)]
    include_windows: bool,
}

#[derive(Serialize)]
struct Manifest {
    schema_version: u8,
    app_id: &'static str,
    version: String,
    tag: String,
    channel: &'static str,
    published_at: String,
    release_url: String,
    assets: Vec<Asset>,
}

#[derive(Serialize)]
struct Asset {
    name: String,
    url: String,
    signature_url: String,
    os: &'static str,
    arch: String,
    format: &'static str,
    content_type: &'static str,
    size: u64,
    sha256: String,
    minimum_os_version: &'static str,
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
    let version = args.tag.strip_prefix('v').unwrap_or(&args.tag).to_string();
    let release_base = format!(
        "{}/releases/{}",
        args.base_url.trim_end_matches('/'),
        args.tag
    );
    let assets = collect_assets(&args.dist, &release_base, args.include_windows)?;
    // The DMGs are the publish gate's guaranteed artifact set; the Windows
    // legs are best-effort per arch (a failed leg publishes without them), so
    // their absence must not sink the whole manifest.
    if !assets.iter().any(|asset| asset.os == "macos") {
        bail!("no architecture-specific DMG assets found for manifest");
    }

    let manifest = Manifest {
        schema_version: 1,
        app_id: APP_ID,
        version,
        tag: args.tag.clone(),
        channel: CHANNEL,
        published_at: OffsetDateTime::from(SystemTime::now())
            .format(&Rfc3339)
            .context("could not format current timestamp")?,
        release_url: format!(
            "https://github.com/AprilNEA/OpenLogi/releases/tag/{}",
            args.tag
        ),
        assets,
    };

    if let Some(parent) = args.output.parent() {
        fs_err::create_dir_all(parent)
            .with_context(|| format!("could not create manifest directory {}", parent.display()))?;
    }
    fs_err::write(
        &args.output,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .with_context(|| format!("could not write manifest to {}", args.output.display()))
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
            os: classified.os,
            arch: classified.arch,
            format: classified.format,
            content_type: classified.content_type,
            size: path
                .metadata()
                .with_context(|| format!("could not stat {}", path.display()))?
                .len(),
            sha256: path
                .sha256()
                .with_context(|| format!("could not hash artifact {}", path.display()))?,
            minimum_os_version: classified.minimum_os_version,
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
#[allow(clippy::unwrap_used, reason = "unwrap is idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn collect_assets_requires_minisign_signature_for_each_dmg() {
        let dist = tempfile::tempdir().unwrap();
        fs_err::write(dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();

        assert!(
            collect_assets(
                dist.path(),
                "https://updates.example/releases/v1.2.3",
                false
            )
            .is_err()
        );
    }

    #[test]
    fn collect_assets_publishes_signature_url() {
        let dist = tempfile::tempdir().unwrap();
        fs_err::write(dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();
        fs_err::write(
            dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg.minisig"),
            b"signature",
        )
        .unwrap();

        let assets = collect_assets(
            dist.path(),
            "https://updates.example/releases/v1.2.3",
            false,
        )
        .unwrap();

        assert_eq!(
            assets[0].signature_url,
            "https://updates.example/releases/v1.2.3/OpenLogi-v1.2.3-macos-arm64.dmg.minisig"
        );
    }

    #[test]
    fn collect_assets_skips_windows_artifacts_unless_opted_in() {
        // Off by default: the manifest must never reference Windows objects
        // the release workflow's R2 upload step doesn't ship.
        let dist = tempfile::tempdir().unwrap();
        for name in [
            "OpenLogi-v1.2.3-windows-x86_64.msi",
            "OpenLogi-v1.2.3-windows-x86_64.zip",
        ] {
            fs_err::write(dist.path().join(name), b"artifact").unwrap();
            fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
        }

        let assets = collect_assets(
            dist.path(),
            "https://updates.example/releases/v1.2.3",
            false,
        )
        .unwrap();

        assert!(assets.is_empty());
    }

    #[test]
    fn collect_assets_includes_windows_msi_and_zip_per_arch() {
        let dist = tempfile::tempdir().unwrap();
        for name in [
            "OpenLogi-v1.2.3-windows-x86_64.msi",
            "OpenLogi-v1.2.3-windows-arm64.msi",
            "OpenLogi-v1.2.3-windows-x86_64.zip",
        ] {
            fs_err::write(dist.path().join(name), b"artifact").unwrap();
            fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
        }

        let assets =
            collect_assets(dist.path(), "https://updates.example/releases/v1.2.3", true).unwrap();

        assert_eq!(assets.len(), 3);
        assert!(assets.iter().all(|a| a.os == "windows"));
        let msi = assets
            .iter()
            .find(|a| a.name.ends_with("x86_64.msi"))
            .unwrap();
        assert_eq!((msi.arch.as_str(), msi.format), ("x86_64", "msi"));
        let zip = assets
            .iter()
            .find(|a| a.name.ends_with("x86_64.zip"))
            .unwrap();
        assert_eq!((zip.arch.as_str(), zip.format), ("x86_64", "zip"));
        assert!(assets.iter().any(|a| a.arch == "arm64"));
    }

    #[test]
    fn collect_assets_skips_linux_packages_and_foreign_archives() {
        let dist = tempfile::tempdir().unwrap();
        for name in [
            "openlogi-v1.2.3-linux-amd64.deb",
            "openlogi-v1.2.3-linux-amd64.rpm",
            "not-an-artifact.zip",
            "SHA256SUMS",
        ] {
            fs_err::write(dist.path().join(name), b"artifact").unwrap();
            fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
        }

        let assets =
            collect_assets(dist.path(), "https://updates.example/releases/v1.2.3", true).unwrap();

        assert!(assets.is_empty());
    }
}

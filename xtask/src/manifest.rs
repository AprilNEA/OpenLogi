use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const APP_ID: &str = "org.openlogi.openlogi";
const CHANNEL: &str = "stable";
const MINIMUM_OS_VERSION: &str = "13.0";

#[derive(Parser)]
pub(crate) struct GenerateUpdaterManifest {
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

pub(crate) fn generate_updater_manifest(args: &GenerateUpdaterManifest) -> Result<()> {
    let version = args.tag.strip_prefix('v').unwrap_or(&args.tag).to_string();
    let release_base = format!(
        "{}/releases/{}",
        args.base_url.trim_end_matches('/'),
        args.tag
    );
    let assets = collect_assets(&args.dist, &release_base)?;
    if assets.is_empty() {
        bail!("no architecture-specific DMG assets found for manifest");
    }

    let manifest = Manifest {
        schema_version: 1,
        app_id: APP_ID,
        version,
        tag: args.tag.clone(),
        channel: CHANNEL,
        published_at: published_at()?,
        release_url: format!(
            "https://github.com/AprilNEA/OpenLogi/releases/tag/{}",
            args.tag
        ),
        assets,
    };

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create manifest directory {}", parent.display()))?;
    }
    fs::write(
        &args.output,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .with_context(|| format!("could not write manifest to {}", args.output.display()))
}

fn collect_assets(dist: &Path, release_base: &str) -> Result<Vec<Asset>> {
    let mut assets = Vec::new();
    for entry in fs::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("dmg") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(arch) = dmg_arch(name) else {
            continue;
        };
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
            os: "macos",
            arch: arch.to_string(),
            format: "dmg",
            content_type: "application/x-apple-diskimage",
            size: path
                .metadata()
                .with_context(|| format!("could not stat {}", path.display()))?
                .len(),
            sha256: sha256(&path)?,
            minimum_os_version: MINIMUM_OS_VERSION,
        });
    }
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(assets)
}

fn dmg_arch(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".dmg")?;
    let (_, arch) = stem.rsplit_once("-macos-")?;
    matches!(arch, "arm64" | "x86_64").then_some(arch)
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("could not open artifact {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let len = file
            .read(&mut buffer)
            .with_context(|| format!("could not read artifact {}", path.display()))?;
        if len == 0 {
            break;
        }
        hasher.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn published_at() -> Result<String> {
    OffsetDateTime::from(SystemTime::now())
        .format(&Rfc3339)
        .context("could not format current timestamp")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{env, fs, process};

    use super::*;

    fn temp_dist(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("openlogi-manifest-test-{}-{name}", process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn collect_assets_requires_minisign_signature_for_each_dmg() {
        let dist = temp_dist("missing-signature");
        fs::write(dist.join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();

        assert!(collect_assets(&dist, "https://updates.example/releases/v1.2.3").is_err());

        let _ = fs::remove_dir_all(dist);
    }

    #[test]
    fn collect_assets_publishes_signature_url() {
        let dist = temp_dist("with-signature");
        fs::write(dist.join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();
        fs::write(
            dist.join("OpenLogi-v1.2.3-macos-arm64.dmg.minisig"),
            b"signature",
        )
        .unwrap();

        let assets = collect_assets(&dist, "https://updates.example/releases/v1.2.3").unwrap();

        assert_eq!(
            assets[0].signature_url,
            "https://updates.example/releases/v1.2.3/OpenLogi-v1.2.3-macos-arm64.dmg.minisig"
        );

        let _ = fs::remove_dir_all(dist);
    }
}

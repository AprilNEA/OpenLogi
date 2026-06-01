//! Blocking HTTP fetch + SHA-256 verification helpers.
//!
//! [`AssetClient`] wraps a single reused [`ureq::Agent`] — one connection
//! pool and TLS session kept alive across the many per-file pulls a sync
//! performs — plus the shared User-Agent and connect-timeout policy.
//! Construct one per sync (per host) and call its `fetch_*` methods in a
//! loop. Used by both the GUI runtime sync (per-device pull) and the CLI
//! bundle sync (all-devices pull).
//!
//! The free functions below are stateless hash / local-file helpers with
//! no relation to a host, so they stay off the client.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tracing::debug;
use ureq::Agent;

use crate::index::{FileEntry, Index};

const USER_AGENT: &str = concat!(
    "openlogi-assets/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/AprilNEA/OpenLogi)"
);

/// Filename of the registry at the asset host's root.
const INDEX_NAME: &str = "index.json";

/// Bound on DNS + TCP + TLS connect. Deliberately does *not* cap body-read
/// time, so a slow-but-progressing download of a large asset isn't killed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Blocking client for one asset host.
///
/// Holds a reused [`ureq::Agent`], so the dozens-to-hundreds of small file
/// pulls a sync makes against the same host share one keep-alive connection
/// instead of paying a fresh TCP + TLS handshake each time.
pub struct AssetClient {
    /// Normalised origin, trailing slash trimmed once at construction.
    base: String,
    agent: Agent,
}

/// Outcome of a cache-checked fetch ([`AssetClient::fetch_entry_if_stale`]).
#[derive(Debug)]
pub enum FetchOutcome {
    /// The on-disk file already matched the registry `sha256`; no download.
    CacheHit,
    /// The file was (re)downloaded; carries the byte count written.
    Fetched { bytes: usize },
}

impl AssetClient {
    /// Build a client for `base` (e.g. `https://assets.openlogi.org`).
    #[must_use]
    pub fn new(base: &str) -> Self {
        let agent: Agent = Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .build()
            .into();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            agent,
        }
    }

    /// GET `<base>/index.json` and parse it.
    pub fn fetch_index(&self) -> Result<Index> {
        Ok(self.fetch_index_raw()?.1)
    }

    /// GET `<base>/index.json`, returning both the raw bytes (so callers can
    /// persist them verbatim) and the parsed struct.
    pub fn fetch_index_raw(&self) -> Result<(Vec<u8>, Index)> {
        let url = format!("{}/{INDEX_NAME}", self.base);
        debug!(%url, "fetching index.json");
        let body = self.get_bytes(&url)?;
        let parsed: Index = serde_json::from_slice(&body).context("parse fetched index.json")?;
        Ok((body, parsed))
    }

    /// Fetch `<base>/index.json`, write it into `dir`, and return the parsed index.
    pub fn fetch_index_to_dir(&self, dir: &Path) -> Result<Index> {
        let (raw, index) = self.fetch_index_raw()?;
        let local = dir.join(INDEX_NAME);
        fs::write(&local, &raw).with_context(|| format!("write {}", local.display()))?;
        Ok(index)
    }

    /// GET a per-depot file, e.g.
    /// `fetch_file("v1/devices/mx_master_4/", "front_core.png")`.
    pub fn fetch_file(&self, asset_path: &str, name: &str) -> Result<Vec<u8>> {
        let asset_path = asset_path.trim_start_matches('/');
        let url = format!("{}/{asset_path}{name}", self.base);
        debug!(%url, "fetching file");
        self.get_bytes(&url)
    }

    /// Fetch a per-depot file into `dir`, returning the number of bytes written.
    pub fn fetch_file_to_dir(&self, asset_path: &str, dir: &Path, name: &str) -> Result<usize> {
        let dst = safe_component_path(dir, name, "asset file name")?;
        reject_symlink(&dst)?;
        let bytes = self.fetch_file(asset_path, name)?;
        fs::write(&dst, &bytes).with_context(|| format!("write {}", dst.display()))?;
        Ok(bytes.len())
    }

    /// Fetch `file` into `dir` unless a file already there matches its
    /// `sha256`. The cache-skip primitive shared by the CLI bundle sync and
    /// the GUI runtime sync — callers branch on [`FetchOutcome`] to do their
    /// own progress reporting.
    pub fn fetch_entry_if_stale(
        &self,
        asset_path: &str,
        dir: &Path,
        file: &FileEntry,
    ) -> Result<FetchOutcome> {
        let dst = safe_component_path(dir, &file.name, "asset file name")?;
        reject_symlink(&dst)?;
        if cached_matches(&dst, &file.sha256) {
            return Ok(FetchOutcome::CacheHit);
        }
        let bytes = self.fetch_file_to_dir(asset_path, dir, &file.name)?;
        if !cached_matches(&dst, &file.sha256) {
            let _ = fs::remove_file(&dst);
            bail!("downloaded asset checksum mismatch: {}", dst.display());
        }
        Ok(FetchOutcome::Fetched { bytes })
    }

    /// GET `url` on the shared agent and read the whole body into memory.
    /// `read_to_vec` caps the body at ureq's default 10 MB — ample for the
    /// registry JSON and the device PNGs, and a safety net against a
    /// runaway response.
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?
            .body_mut()
            .read_to_vec()
            .with_context(|| format!("read body {url}"))
    }
}

/// Load and parse a JSON document from disk.
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Raw bytes of `path`. Avoid for very large files — held entirely in
/// memory.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

/// Join one untrusted registry component to a trusted directory.
///
/// Remote asset metadata is expected to carry depot and file *names*, not
/// paths. Rejecting separators, absolute prefixes, and `..` keeps asset syncs
/// inside the cache or bundle directory chosen by the caller.
pub fn safe_component_path(base: &Path, component: &str, label: &str) -> Result<PathBuf> {
    ensure_safe_component(component, label)?;
    Ok(base.join(component))
}

fn ensure_safe_component(component: &str, label: &str) -> Result<()> {
    if component.is_empty() {
        bail!("{label} is empty");
    }
    if component.contains('/') || component.contains('\\') {
        bail!("{label} must be a single path component: {component}");
    }
    let mut parts = Path::new(component).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("{label} must be a safe relative path component: {component}"),
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        bail!(
            "refusing to write asset through symlink: {}",
            path.display()
        );
    }
    Ok(())
}

/// Hex SHA-256 of an in-memory blob.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Streamed hex SHA-256 of `path`.
pub fn sha256_of_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Returns true when `path` exists and its SHA-256 matches `expected_sha`
/// (case-insensitive). Any error opening or reading silently returns
/// `false` — callers re-fetch instead of erroring out.
#[must_use]
pub fn cached_matches(path: &Path, expected_sha: &str) -> bool {
    sha256_of_file(path).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected_sha))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn safe_component_path_accepts_plain_names() {
        assert_eq!(
            safe_component_path(Path::new("/cache"), "front_core.png", "asset").unwrap(),
            Path::new("/cache").join("front_core.png")
        );
    }

    #[test]
    fn safe_component_path_rejects_traversal_and_separators() {
        for name in [
            "",
            ".",
            "..",
            "../LaunchAgents/x",
            "nested/file.png",
            "nested\\file.png",
        ] {
            assert!(safe_component_path(Path::new("/cache"), name, "asset").is_err());
        }
    }
}

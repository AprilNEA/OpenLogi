//! Blocking HTTP fetch + SHA-256 verification helpers.
//!
//! Used by both the GUI runtime sync (per-device pull) and the CLI
//! bundle sync (all-devices pull). Keeps a single User-Agent string
//! and one shared retry / timeout policy.

use std::fs;
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::index::Index;

const USER_AGENT: &str = concat!(
    "openlogi-assets/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/AprilNEA/OpenLogi)"
);

/// Filename of the registry at the asset host's root.
const INDEX_NAME: &str = "index.json";

/// Load and parse a JSON document from disk.
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// GET `<base>/index.json` and parse it.
pub fn fetch_index(base: &str) -> Result<Index> {
    let (_, parsed) = fetch_index_raw(base)?;
    Ok(parsed)
}

/// GET `<base>/index.json`, returning both the raw bytes (so callers can
/// persist them verbatim) and the parsed struct.
pub fn fetch_index_raw(base: &str) -> Result<(Vec<u8>, Index)> {
    let base = base.trim_end_matches('/');
    let url = format!("{base}/{INDEX_NAME}");
    debug!(%url, "fetching index.json");
    let body = get_bytes(&url)?;
    let parsed: Index = serde_json::from_slice(&body).context("parse fetched index.json")?;
    Ok((body, parsed))
}

/// Fetch `<base>/index.json`, write it into `dir`, and return the parsed index.
pub fn fetch_index_to_dir(base: &str, dir: &Path) -> Result<Index> {
    let (raw, index) = fetch_index_raw(base)?;
    let local = dir.join(INDEX_NAME);
    fs::write(&local, &raw).with_context(|| format!("write {}", local.display()))?;
    Ok(index)
}

/// GET a per-depot file, e.g.
/// `fetch_file("https://assets.openlogi.org", "v1/devices/mx_master_4/", "front_core.png")`.
pub fn fetch_file(base: &str, asset_path: &str, name: &str) -> Result<Vec<u8>> {
    let base = base.trim_end_matches('/');
    let asset_path = asset_path.trim_start_matches('/');
    let url = format!("{base}/{asset_path}{name}");
    debug!(%url, "fetching file");
    get_bytes(&url)
}

/// Fetch a per-depot file into `dir`, returning the number of bytes written.
pub fn fetch_file_to_dir(base: &str, asset_path: &str, dir: &Path, name: &str) -> Result<usize> {
    let dst = dir.join(name);
    let bytes = fetch_file(base, asset_path, name)?;
    fs::write(&dst, &bytes).with_context(|| format!("write {}", dst.display()))?;
    Ok(bytes.len())
}

/// Raw bytes of `path`. Avoid for very large files — held entirely in
/// memory.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

/// Hex SHA-256 of an in-memory blob.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Streamed hex SHA-256 of `path`.
pub fn sha256_of_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Returns true when `path` exists and its SHA-256 matches `expected_sha`
/// (case-insensitive). Any error opening or reading silently returns
/// `false` — callers re-fetch instead of erroring out.
#[must_use]
pub fn cached_matches(path: &Path, expected_sha: &str) -> bool {
    sha256_of_file(path).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected_sha))
}

fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .header("user-agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .with_context(|| format!("read body {url}"))?;
    Ok(body)
}

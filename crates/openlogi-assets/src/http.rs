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

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use backon::{BlockingRetryable, ExponentialBuilder};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use ureq::Agent;

use crate::error::AssetError;
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

/// Retries after the initial attempt for a single GET (3 tries total).
const MAX_RETRIES: usize = 2;

/// Backoff before the first retry; doubles each attempt (200ms, 400ms,
/// plus jitter). Keeps a transient blip from needing an app restart
/// without a fleet of clients hammering the host in lockstep.
const RETRY_MIN_DELAY: Duration = Duration::from_millis(200);

/// Blocking client for one asset host.
///
/// Holds a reused [`ureq::Agent`], so the dozens-to-hundreds of small file
/// pulls a sync makes against the same host share one keep-alive connection
/// instead of paying a fresh TCP + TLS handshake each time.
pub struct AssetClient {
    location: AssetLocation,
    agent: Agent,
}

enum AssetLocation {
    Uniform {
        base: String,
    },
    JsDelivr {
        catalog_base: String,
        package_root: String,
        package_version: String,
        package_by_asset_path: HashMap<String, String>,
    },
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
        Self {
            location: AssetLocation::Uniform {
                base: base.trim_end_matches('/').to_owned(),
            },
            agent: Self::agent(),
        }
    }

    pub(crate) fn new_jsdelivr(
        catalog_base: &str,
        package_root: &str,
        package_version: &str,
        package_by_asset_path: HashMap<String, String>,
    ) -> Self {
        Self {
            location: AssetLocation::JsDelivr {
                catalog_base: catalog_base.trim_end_matches('/').to_owned(),
                package_root: package_root.trim_end_matches('/').to_owned(),
                package_version: package_version.to_owned(),
                package_by_asset_path,
            },
            agent: Self::agent(),
        }
    }

    fn agent() -> Agent {
        Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .build()
            .into()
    }

    /// GET `<base>/index.json` and parse it.
    pub fn fetch_index(&self) -> Result<Index, AssetError> {
        Ok(self.fetch_index_raw()?.1)
    }

    /// GET `<base>/index.json`, returning both the raw bytes (so callers can
    /// persist them verbatim) and the parsed struct.
    pub fn fetch_index_raw(&self) -> Result<(Vec<u8>, Index), AssetError> {
        let url = self.index_url();
        debug!(%url, "fetching index.json");
        let body = self.get_bytes(&url)?;
        let parsed: Index =
            serde_json::from_slice(&body).map_err(|source| AssetError::ParseJson {
                what: "fetched index.json".to_owned(),
                source,
            })?;
        Ok((body, parsed))
    }

    /// Fetch `<base>/index.json`, write it into `dir`, and return the parsed index.
    pub fn fetch_index_to_dir(&self, dir: &Path) -> Result<Index, AssetError> {
        let (raw, index) = self.fetch_index_raw()?;
        write_replace(&dir.join(INDEX_NAME), &raw)?;
        Ok(index)
    }

    /// GET a per-depot file, e.g.
    /// `fetch_file("v1/devices/mx_master_4/", "front_core.png")`.
    fn fetch_file(&self, asset_path: &str, name: &str) -> Result<Vec<u8>, AssetError> {
        let url = self.asset_url(asset_path, name)?;
        debug!(%url, "fetching file");
        self.get_bytes(&url)
    }

    fn index_url(&self) -> String {
        let base = match &self.location {
            AssetLocation::Uniform { base } => base,
            AssetLocation::JsDelivr { catalog_base, .. } => catalog_base,
        };
        format!("{base}/{INDEX_NAME}")
    }

    pub(crate) fn asset_url(&self, asset_path: &str, name: &str) -> Result<String, AssetError> {
        let asset_path = asset_path.trim_start_matches('/');
        match &self.location {
            AssetLocation::Uniform { base } => Ok(format!("{base}/{asset_path}{name}")),
            AssetLocation::JsDelivr {
                package_root,
                package_version,
                package_by_asset_path,
                ..
            } => {
                let package = package_by_asset_path.get(asset_path).ok_or_else(|| {
                    AssetError::MissingNpmAssetPath {
                        asset_path: asset_path.to_owned(),
                    }
                })?;
                Ok(format!(
                    "{package_root}/{package}@{package_version}/{asset_path}{name}"
                ))
            }
        }
    }

    /// Fetch `file` into `dir` unless a file already there matches its
    /// `sha256`. The download is verified against the expected hash *in
    /// memory, before it is written*, so only correct bytes ever reach the
    /// cache directory and a mismatch leaves any existing file untouched. The
    /// cache-skip primitive shared by the CLI bundle sync and the GUI
    /// runtime sync — callers branch on [`FetchOutcome`] to do their own
    /// progress reporting.
    pub fn fetch_entry_if_stale(
        &self,
        asset_path: &str,
        dir: &Path,
        file: &FileEntry,
    ) -> Result<FetchOutcome, AssetError> {
        // `name` comes from remote metadata; validate it down to a single
        // path component before any path is built.
        let dst = safe_component_path(dir, &file.name, "asset file name")?;
        if cached_matches(&dst, &file.sha256) {
            return Ok(FetchOutcome::CacheHit);
        }
        let bytes = self.fetch_file(asset_path, &file.name)?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(AssetError::ChecksumMismatch {
                name: file.name.clone(),
                expected: file.sha256.clone(),
                actual,
            });
        }
        write_replace(&dst, &bytes)?;
        Ok(FetchOutcome::Fetched { bytes: bytes.len() })
    }

    /// GET `url` on the shared agent and read the whole body into memory,
    /// retrying transient failures (timeouts, dropped connections, 5xx) with
    /// exponential backoff. Permanent failures (4xx, malformed request) fail
    /// fast. `read_to_vec` caps the body at ureq's default 10 MB — ample for
    /// the registry JSON and the device PNGs, and a safety net against a
    /// runaway response.
    ///
    /// The backoff sleeps block the calling thread, which is fine: every
    /// caller runs on the sync's dedicated background thread, never the
    /// async runtime. `backon` defaults to `std::thread::sleep` here.
    pub(crate) fn get_bytes(&self, url: &str) -> Result<Vec<u8>, AssetError> {
        let policy = ExponentialBuilder::default()
            .with_min_delay(RETRY_MIN_DELAY)
            .with_factor(2.0)
            .with_max_times(MAX_RETRIES)
            .with_jitter();
        (|| self.try_get_bytes(url))
            .retry(policy)
            .when(is_retryable)
            .notify(|e: &ureq::Error, dur: Duration| {
                warn!(%url, backoff_ms = dur.as_millis(), error = ?e, "transient fetch error — retrying");
            })
            .call()
            .map_err(|source| AssetError::Http {
                url: url.to_owned(),
                source,
            })
    }

    /// One GET + full body read, surfacing the typed [`ureq::Error`] so the
    /// retry loop in [`get_bytes`](Self::get_bytes) can tell transient
    /// failures from permanent ones.
    fn try_get_bytes(&self, url: &str) -> std::result::Result<Vec<u8>, ureq::Error> {
        self.agent.get(url).call()?.body_mut().read_to_vec()
    }
}

/// Whether a failed fetch is worth retrying. Transport-level hiccups
/// (timeouts, dropped/refused connections, DNS blips) and 5xx — plus the two
/// "back off and retry" 4xx codes — are transient; a 4xx like 404 or a
/// malformed-request error won't change on a retry.
fn is_retryable(error: &ureq::Error) -> bool {
    use ureq::Error;
    match error {
        Error::StatusCode(code) => *code >= 500 || matches!(*code, 408 | 429),
        Error::Io(_)
        | Error::Timeout(_)
        | Error::ConnectionFailed
        | Error::HostNotFound
        | Error::Protocol(_) => true,
        _ => false,
    }
}

/// Load and parse a JSON document from disk.
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, AssetError> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).map_err(|source| AssetError::ParseJson {
        what: path.display().to_string(),
        source,
    })
}

/// Raw bytes of `path`. Avoid for very large files — held entirely in
/// memory.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, AssetError> {
    fs::read(path).map_err(|source| AssetError::ReadFile {
        path: path.to_owned(),
        source,
    })
}

/// Join one untrusted registry component onto a trusted directory.
///
/// Remote asset metadata is expected to carry depot and file *names*, not
/// paths. Rejecting separators, absolute prefixes, and `.`/`..` keeps every
/// sync write inside the cache or bundle directory chosen by the caller.
pub fn safe_component_path(
    base: &Path,
    component: &str,
    label: &str,
) -> Result<PathBuf, AssetError> {
    let reject = || AssetError::UnsafeComponent {
        label: label.to_owned(),
        component: component.to_owned(),
    };
    // `Path::components` never yields separators on the platform that didn't
    // produce them, so reject both kinds explicitly before consulting it.
    if component.is_empty() || component.contains('/') || component.contains('\\') {
        return Err(reject());
    }
    let mut parts = Path::new(component).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => Ok(base.join(component)),
        _ => Err(reject()),
    }
}

/// Write `bytes` beside `dst` and atomically rename into place.
///
/// The temporary file is created in the destination directory and committed via
/// rename, so a concurrent reader sees the old file or the new one, never a
/// half-written one. A planted symlink at `dst` is replaced, not followed.
pub(crate) fn write_replace(dst: &Path, bytes: &[u8]) -> Result<(), AssetError> {
    use std::io::Write as _;

    let fail = |source| AssetError::WriteFile {
        path: dst.to_owned(),
        source,
    };
    let mut file = AtomicWriteFile::open(dst).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    file.commit().map_err(fail)
}

/// Hex SHA-256 of an in-memory blob.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Streamed hex SHA-256 of `path`.
pub fn sha256_of_file(path: &Path) -> Result<String, AssetError> {
    let fail = |source| AssetError::ReadFile {
        path: path.to_owned(),
        source,
    };
    let mut file = fs::File::open(path).map_err(fail)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(fail)?;
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
mod tests {
    use super::{AssetClient, is_retryable, safe_component_path, write_replace};
    use std::path::Path;
    use ureq::Error;

    #[test]
    fn uniform_source_preserves_the_cloudflare_path() {
        let client = AssetClient::new("https://assets.openlogi.org/");

        assert_eq!(client.index_url(), "https://assets.openlogi.org/index.json");
        assert_eq!(
            client
                .asset_url("v1/devices/mx_master_3s/", "front_core.png")
                .ok()
                .as_deref(),
            Some("https://assets.openlogi.org/v1/devices/mx_master_3s/front_core.png")
        );
    }

    #[test]
    #[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
    fn write_replace_overwrites_in_place() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("a.png");

        write_replace(&dst, b"one").expect("first write");
        write_replace(&dst, b"two").expect("replace");

        assert_eq!(std::fs::read(&dst).expect("read back"), b"two");
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
    fn write_replace_replaces_a_planted_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"untouched").expect("seed victim");
        let dst = dir.path().join("b.png");
        std::os::unix::fs::symlink(&victim, &dst).expect("plant symlink");

        write_replace(&dst, b"payload").expect("write through planted link");

        // The link target must be untouched, and the link itself must now be
        // a regular file holding the payload.
        assert_eq!(std::fs::read(&victim).expect("victim intact"), b"untouched");
        let meta = std::fs::symlink_metadata(&dst).expect("stat dst");
        assert!(meta.file_type().is_file());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"payload");
    }

    #[test]
    fn safe_component_path_accepts_plain_names() {
        assert_eq!(
            safe_component_path(Path::new("/cache"), "front_core.png", "asset").ok(),
            Some(Path::new("/cache").join("front_core.png"))
        );
        assert_eq!(
            safe_component_path(Path::new("/cache"), "mx_master_4", "depot").ok(),
            Some(Path::new("/cache").join("mx_master_4"))
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
            "/etc/passwd",
        ] {
            assert!(
                safe_component_path(Path::new("/cache"), name, "asset").is_err(),
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn retries_transient_failures_not_permanent_ones() {
        // Transient: server errors, the two "back off" 4xx codes, and
        // transport-level failures all warrant a retry.
        assert!(is_retryable(&Error::StatusCode(500)));
        assert!(is_retryable(&Error::StatusCode(503)));
        assert!(is_retryable(&Error::StatusCode(408)));
        assert!(is_retryable(&Error::StatusCode(429)));
        assert!(is_retryable(&Error::HostNotFound));
        assert!(is_retryable(&Error::ConnectionFailed));
        assert!(is_retryable(&Error::Io(
            std::io::ErrorKind::ConnectionReset.into()
        )));

        // Permanent: a missing file or bad request won't change on retry.
        assert!(!is_retryable(&Error::StatusCode(404)));
        assert!(!is_retryable(&Error::StatusCode(400)));
        assert!(!is_retryable(&Error::StatusCode(403)));
    }
}

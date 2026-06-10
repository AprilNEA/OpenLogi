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
use backon::{BlockingRetryable, ExponentialBuilder};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
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
        write_replace(&dir.join(INDEX_NAME), &raw)?;
        Ok(index)
    }

    /// GET a per-depot file, e.g.
    /// `fetch_file("v1/devices/mx_master_4/", "front_core.png")`.
    fn fetch_file(&self, asset_path: &str, name: &str) -> Result<Vec<u8>> {
        let asset_path = asset_path.trim_start_matches('/');
        let url = format!("{}/{asset_path}{name}", self.base);
        debug!(%url, "fetching file");
        self.get_bytes(&url)
    }

    /// Fetch a per-depot file into `dir`, returning the number of bytes
    /// written. `name` comes from remote metadata, so it is validated down
    /// to a single path component before any path is built.
    fn fetch_file_to_dir(&self, asset_path: &str, dir: &Path, name: &str) -> Result<usize> {
        let dst = safe_component_path(dir, name, "asset file name")?;
        let bytes = self.fetch_file(asset_path, name)?;
        write_replace(&dst, &bytes)?;
        Ok(bytes.len())
    }

    /// Fetch `file` into `dir` unless a file already there matches its
    /// `sha256`; a fresh download is verified against the same hash and
    /// removed on mismatch, so nothing unverified survives on disk. The
    /// cache-skip primitive shared by the CLI bundle sync and the GUI
    /// runtime sync — callers branch on [`FetchOutcome`] to do their own
    /// progress reporting.
    pub fn fetch_entry_if_stale(
        &self,
        asset_path: &str,
        dir: &Path,
        file: &FileEntry,
    ) -> Result<FetchOutcome> {
        let dst = safe_component_path(dir, &file.name, "asset file name")?;
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
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
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
            .map_err(|e| anyhow::Error::new(e).context(format!("GET {url}")))
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
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Raw bytes of `path`. Avoid for very large files — held entirely in
/// memory.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

/// Join one untrusted registry component onto a trusted directory.
///
/// Remote asset metadata is expected to carry depot and file *names*, not
/// paths. Rejecting separators, absolute prefixes, and `.`/`..` keeps every
/// sync write inside the cache or bundle directory chosen by the caller.
pub fn safe_component_path(base: &Path, component: &str, label: &str) -> Result<PathBuf> {
    if component.is_empty() {
        bail!("{label} is empty");
    }
    // `Path::components` never yields separators on the platform that didn't
    // produce them, so reject both kinds explicitly before consulting it.
    if component.contains('/') || component.contains('\\') {
        bail!("{label} must be a single path component: {component}");
    }
    let mut parts = Path::new(component).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => Ok(base.join(component)),
        _ => bail!("{label} must be a safe relative path component: {component}"),
    }
}

/// Write `bytes` beside `dst` and atomically rename into place.
///
/// `create_new` refuses to open through anything pre-planted at the temp
/// path (`O_EXCL` never follows symlinks), and `rename` *replaces* a symlink
/// sitting at `dst` instead of writing through it — together they close the
/// check-to-write race a symlink check followed by a plain `fs::write` would
/// leave open. The rename also means a concurrent reader sees the old file
/// or the new one, never a half-written one.
fn write_replace(dst: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let mut tmp_name = dst.as_os_str().to_owned();
    tmp_name.push(".part");
    let tmp = PathBuf::from(tmp_name);
    // A stale `.part` from a crashed sync would fail `create_new`; it never
    // holds verified data, so clear it.
    let _ = fs::remove_file(&tmp);
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    let written = file
        .write_all(bytes)
        .with_context(|| format!("write {}", tmp.display()));
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, dst).with_context(|| format!("replace {}", dst.display()))
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
mod tests {
    use super::{is_retryable, safe_component_path, write_replace};
    use std::path::Path;
    use ureq::Error;

    #[test]
    #[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
    fn write_replace_overwrites_in_place() {
        let dir = std::env::temp_dir().join(format!("openlogi-http-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let dst = dir.join("a.png");

        write_replace(&dst, b"one").expect("first write");
        write_replace(&dst, b"two").expect("replace");

        assert_eq!(std::fs::read(&dst).expect("read back"), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
    fn write_replace_replaces_a_planted_symlink_instead_of_following_it() {
        let dir =
            std::env::temp_dir().join(format!("openlogi-http-symlink-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"untouched").expect("seed victim");
        let dst = dir.join("b.png");
        std::os::unix::fs::symlink(&victim, &dst).expect("plant symlink");

        write_replace(&dst, b"payload").expect("write through planted link");

        // The link target must be untouched, and the link itself must now be
        // a regular file holding the payload.
        assert_eq!(std::fs::read(&victim).expect("victim intact"), b"untouched");
        let meta = std::fs::symlink_metadata(&dst).expect("stat dst");
        assert!(meta.file_type().is_file());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"payload");
        let _ = std::fs::remove_dir_all(&dir);
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

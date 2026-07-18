//! The crate-wide error type for registry fetch, parse, and cache writes.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from asset-registry fetches, JSON parsing, and cache-file I/O.
#[derive(Debug, Error)]
pub enum AssetError {
    /// A GET against the asset host failed, after the retry policy for
    /// transient failures was exhausted.
    #[error("GET {url}")]
    Http {
        /// Full URL of the failed request.
        url: String,
        /// The transport or HTTP-status failure.
        #[source]
        source: ureq::Error,
    },
    /// A registry or metadata JSON document failed to parse.
    #[error("parse {what}")]
    ParseJson {
        /// What was being parsed — a local path or a description of a
        /// just-fetched document.
        what: String,
        /// The underlying deserialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// Opening or reading a local file failed.
    #[error("read {}", path.display())]
    ReadFile {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// Writing a file (via the atomic write-and-rename) failed.
    #[error("write {}", path.display())]
    WriteFile {
        /// The destination that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A downloaded asset's SHA-256 did not match the registry entry, so it
    /// was discarded before reaching the cache.
    #[error("downloaded asset checksum mismatch for {name}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Registry file name of the asset.
        name: String,
        /// The SHA-256 the registry promised.
        expected: String,
        /// The SHA-256 of the bytes actually downloaded.
        actual: String,
    },
    /// A remote-supplied name was not a single safe path component
    /// (empty, contained separators, or was `.`/`..`).
    #[error("{label} must be a single safe path component, got {component:?}")]
    UnsafeComponent {
        /// Which input was rejected (e.g. `"asset file name"`).
        label: String,
        /// The offending value.
        component: String,
    },
}

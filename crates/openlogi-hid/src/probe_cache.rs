//! The file-backed [`ProbeCacheStore`] every native build uses.
//!
//! This is the host half of probe-cache persistence: JSON on disk under the
//! app data dir. Nothing above it names a path.

use std::io;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use serde::de::DeserializeOwned;
use tracing::{debug, warn};

use openlogi_device::inventory::persist::{
    EncodedSnapshot, ProbeCacheError, ProbeCacheSnapshot, ProbeCacheStore, SnapshotRejected,
};

/// Keeps the probe cache as JSON at a fixed path.
pub struct FileProbeCacheStore {
    path: PathBuf,
}

impl FileProbeCacheStore {
    /// A store at `path`.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// A store under the app data dir, or `None` when no data dir resolves —
    /// in which case the caller stays memory-only rather than guessing a path.
    #[must_use]
    pub fn in_data_dir() -> Option<Self> {
        match openlogi_core::paths::data_dir() {
            Ok(dir) => Some(Self::at(dir.join("probe-cache.json"))),
            Err(e) => {
                warn!(error = %e, "no data dir — probe cache is memory-only");
                None
            }
        }
    }

    /// Where this store keeps its snapshot.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ProbeCacheStore for FileProbeCacheStore {
    fn load(&self) -> ProbeCacheSnapshot {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return ProbeCacheSnapshot::empty();
        };
        match ProbeCacheSnapshot::read(&Json(&bytes)) {
            Ok(snapshot) => snapshot,
            Err(SnapshotRejected::ForeignSchema { version }) => {
                debug!(
                    path = ?self.path,
                    version,
                    "The probe cache was written with another schema, so it starts empty. This is not a problem: devices are probed afresh and the cache is rebuilt."
                );
                ProbeCacheSnapshot::empty()
            }
            Err(SnapshotRejected::Unreadable { reason }) => {
                warn!(
                    path = ?self.path,
                    %reason,
                    "The probe cache file could not be decoded, so it starts empty. This is safe to ignore: devices are probed afresh and the cache is rebuilt."
                );
                ProbeCacheSnapshot::empty()
            }
        }
    }

    /// Written atomically via [`AtomicWriteFile`] so a crash mid-write can't
    /// leave a torn file — and so the replace works on Windows, where a plain
    /// rename onto an existing file fails.
    fn save(&self, snapshot: &ProbeCacheSnapshot) -> Result<(), ProbeCacheError> {
        self.write(snapshot)
            .map_err(|e| ProbeCacheError::new(format!("{}: {e}", self.path.display())))
    }
}

/// The bytes of a snapshot this store kept, in its JSON encoding.
struct Json<'a>(&'a [u8]);

impl EncodedSnapshot for Json<'_> {
    type Error = serde_json::Error;

    fn decode<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(self.0)
    }
}

impl FileProbeCacheStore {
    fn write(&self, snapshot: &ProbeCacheSnapshot) -> io::Result<()> {
        let json = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = AtomicWriteFile::open(&self.path)?;
        io::Write::write_all(&mut out, &json)?;
        out.commit()
    }
}

#[cfg(test)]
mod tests {
    use openlogi_device::inventory::persist::{ProbeCacheSnapshot, ProbeCacheStore as _};

    use super::FileProbeCacheStore;

    /// The store is a warm-start optimization, so anything it cannot read is a
    /// cold start — never an error that reaches device discovery.
    #[test]
    fn load_tolerates_a_missing_or_unreadable_file() {
        let dir = tempfile::tempdir().expect("tempdir");

        let missing = FileProbeCacheStore::at(dir.path().join("nope.json"));
        assert!(missing.load().is_empty(), "a missing file is a cold start");

        let path = dir.path().join("garbage.json");
        std::fs::write(&path, b"not json at all").expect("write");
        assert!(
            FileProbeCacheStore::at(path).load().is_empty(),
            "a torn write is a cold start, not a panic"
        );
    }

    /// Writing goes through a directory that may not exist yet — the app data
    /// dir on a first run.
    #[test]
    fn save_creates_the_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileProbeCacheStore::at(dir.path().join("nested/deep/probe-cache.json"));

        store.save(&ProbeCacheSnapshot::empty()).expect("save");

        assert!(store.path().is_file(), "the snapshot must land on disk");
    }
}

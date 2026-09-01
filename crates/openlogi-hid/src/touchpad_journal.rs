//! File-backed ownership journal for volatile touchpad raw-report mode.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use atomic_write_file::AtomicWriteFile;
use openlogi_device::session::gesture::{
    RawModeJournal, TouchpadJournalError, TouchpadJournalStore,
};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

/// Durable raw-mode ownership records stored under the application state dir.
///
/// All devices share one atomically replaced JSON document. The in-process
/// mutex serializes concurrent session updates so one device cannot overwrite
/// another device's crash-recovery record.
pub struct FileTouchpadJournalStore {
    path: PathBuf,
    journals: Mutex<JournalFile>,
}

#[derive(Default, Serialize, Deserialize)]
struct JournalFile {
    version: u32,
    entries: BTreeMap<String, RawModeJournal>,
}

impl FileTouchpadJournalStore {
    /// Open a journal at `path`. A missing file starts empty; malformed or
    /// foreign-schema content is an error because discarding it could leave a
    /// device in raw mode with no recovery record.
    pub fn at(path: PathBuf) -> Result<Self, TouchpadJournalError> {
        let journals = match std::fs::read(&path) {
            Ok(bytes) => {
                let parsed: JournalFile =
                    serde_json::from_slice(&bytes).map_err(|error| journal_error(&path, error))?;
                if parsed.version != SCHEMA_VERSION {
                    return Err(TouchpadJournalError::new(format_args!(
                        "{}: unsupported journal schema {}",
                        path.display(),
                        parsed.version
                    )));
                }
                parsed
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => JournalFile {
                version: SCHEMA_VERSION,
                entries: BTreeMap::new(),
            },
            Err(error) => return Err(journal_error(&path, error)),
        };
        Ok(Self {
            path,
            journals: Mutex::new(journals),
        })
    }

    /// Open the native application's journal under its state directory.
    pub fn in_state_dir() -> Result<Self, TouchpadJournalError> {
        let path = openlogi_core::paths::state_dir()
            .map_err(TouchpadJournalError::new)?
            .join("touchpad-raw-mode.json");
        Self::at(path)
    }

    /// Journal file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stable device identities whose raw-mode ownership has not yet been
    /// resolved. An existing but empty journal file returns an empty list.
    pub fn pending_ids(&self) -> Result<Vec<String>, TouchpadJournalError> {
        let journals = self
            .journals
            .lock()
            .map_err(|error| TouchpadJournalError::new(error.to_string()))?;
        Ok(journals.entries.keys().cloned().collect())
    }

    fn write(&self, journals: &JournalFile) -> Result<(), TouchpadJournalError> {
        let json = serde_json::to_vec(journals).map_err(TouchpadJournalError::new)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| journal_error(&self.path, error))?;
        }
        let mut output =
            AtomicWriteFile::open(&self.path).map_err(|error| journal_error(&self.path, error))?;
        io::Write::write_all(&mut output, &json)
            .map_err(|error| journal_error(&self.path, error))?;
        output
            .commit()
            .map_err(|error| journal_error(&self.path, error))
    }
}

impl TouchpadJournalStore for FileTouchpadJournalStore {
    fn load(&self, device_id: &str) -> Result<Option<RawModeJournal>, TouchpadJournalError> {
        let journals = self
            .journals
            .lock()
            .map_err(|error| TouchpadJournalError::new(error.to_string()))?;
        Ok(journals.entries.get(device_id).copied())
    }

    fn save(&self, device_id: &str, journal: RawModeJournal) -> Result<(), TouchpadJournalError> {
        let mut journals = self.journals.lock().unwrap_or_else(PoisonError::into_inner);
        journals.entries.insert(device_id.to_string(), journal);
        self.write(&journals)
    }

    fn clear(&self, device_id: &str) -> Result<(), TouchpadJournalError> {
        let mut journals = self.journals.lock().unwrap_or_else(PoisonError::into_inner);
        journals.entries.remove(device_id);
        self.write(&journals)
    }
}

fn journal_error(path: &Path, error: impl std::fmt::Display) -> TouchpadJournalError {
    TouchpadJournalError::new(format_args!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journals_survive_reopen_and_clear_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/touchpad.json");
        let journal = RawModeJournal {
            original: 0,
            requested: 5,
            readback: Some(5),
            armed: true,
            scroll2finger_diverted: Some(false),
        };
        let store = FileTouchpadJournalStore::at(path.clone()).expect("store");
        store.save("unit:12345678", journal).expect("save");

        let reopened = FileTouchpadJournalStore::at(path).expect("reopen");
        assert_eq!(reopened.load("unit:12345678").expect("load"), Some(journal));
        assert_eq!(
            reopened.pending_ids().expect("pending ids"),
            vec!["unit:12345678"]
        );
        reopened.clear("unit:12345678").expect("clear");
        assert_eq!(reopened.load("unit:12345678").expect("load"), None);
        assert!(reopened.pending_ids().expect("pending ids").is_empty());
    }

    #[test]
    fn malformed_journal_is_not_silently_discarded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("touchpad.json");
        std::fs::write(&path, b"not json").expect("write");

        assert!(FileTouchpadJournalStore::at(path).is_err());
    }
}

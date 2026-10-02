//! A single writer for bounded, versioned agent state with external-edit detection.

use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use openlogi_core::peripheral::PeripheralError;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone)]
pub(super) struct StateFile {
    path: PathBuf,
    source: Option<Vec<u8>>,
}

impl StateFile {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path, source: None }
    }

    pub(super) fn load<T: DeserializeOwned>(&mut self) -> Result<Option<T>, PeripheralError> {
        self.source = read(&self.path)?;
        self.source
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|e| failed(&e))
    }

    pub(super) fn save<T: Serialize>(&mut self, document: &T) -> Result<(), PeripheralError> {
        if read(&self.path)? != self.source {
            return Err(PeripheralError::JournalFailed(
                "state changed outside its owning agent".into(),
            ));
        }
        let body = serde_json::to_vec(document).map_err(|e| failed(&e))?;
        if body.len() > 4 * 1024 * 1024 {
            return Err(PeripheralError::JournalFailed(
                "agent state exceeds 4 MiB".into(),
            ));
        }
        if self.source.as_ref() == Some(&body) {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| failed(&e))?;
        }
        #[cfg_attr(
            not(unix),
            expect(unused_mut, reason = "only Unix sets file permissions")
        )]
        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut file = options.open(&self.path).map_err(|e| failed(&e))?;
        file.write_all(&body).map_err(|e| failed(&e))?;
        file.commit().map_err(|e| failed(&e))?;
        self.source = Some(body);
        Ok(())
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, PeripheralError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(&error)),
    };
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| failed(&e))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(PeripheralError::JournalFailed(
            "agent state exceeds 4 MiB".into(),
        ));
    }
    Ok(Some(bytes))
}

fn failed(error: &dyn std::fmt::Display) -> PeripheralError {
    PeripheralError::JournalFailed(error.to_string())
}

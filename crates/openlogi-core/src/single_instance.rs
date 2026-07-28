//! Cross-platform single-instance process guard.
//!
//! On startup a process tries to acquire an exclusive, non-blocking lock on a
//! named file under the user's data dir. Holding the lock keeps a second
//! invocation of the *same* role from running — the GUI uses it to avoid a
//! duplicate window, the background agent to avoid two processes fighting over
//! the same devices and IPC socket. Each role passes its own lock file name so
//! the GUI and the agent don't lock each other out. The lock is released by the
//! OS when the process exits, so crash-recovery is free: the next launch
//! reclaims the lock on the leftover file without any cleanup ceremony.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::PathBuf,
};

use thiserror::Error;
use tracing::debug;

use crate::paths::{self, PathsError};

/// Held by `main` for the duration of the run; dropped on exit (the OS
/// releases the underlying file lock at the same time). The `_handle` field
/// is intentionally unused — the value is alive only for its `Drop` side
/// effect of closing the fd.
#[allow(
    dead_code,
    reason = "the File is held only so the OS keeps the lock — not read again"
)]
pub struct InstanceGuard {
    _handle: File,
}

/// Failure acquiring the single-instance lock.
/// [`InstanceError::AlreadyRunning`] is the expected "another copy is open"
/// signal; every other variant indicates filesystem trouble.
#[derive(Debug, Error)]
pub enum InstanceError {
    /// The lock file's directory could not be resolved (no home directory).
    #[error("could not resolve lock path")]
    Path(#[from] PathsError),
    /// Creating or opening the lock file failed.
    #[error("could not open lock file at {path}")]
    Open {
        /// The lock file being opened.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Another process of the same role already holds the lock — surface it
    /// politely and exit with a non-error status.
    #[error("another instance already holds the lock at {path}")]
    AlreadyRunning {
        /// The contested lock file.
        path: PathBuf,
    },
    /// The lock syscall itself failed, as opposed to the lock being held.
    #[error("lock attempt at {path} failed")]
    LockFailed {
        /// The lock file the attempt targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Acquire the single-instance lock on `lock_name` (a bare file name resolved
/// under [`paths::config_dir`]). Returns `Ok(guard)` on success — keep the
/// guard alive until the process is about to exit.
///
/// `AlreadyRunning` is the polite "another copy is open" signal callers
/// surface to the user (and exit with a non-error status). Other variants
/// indicate filesystem trouble.
///
/// # Errors
///
/// Returns [`InstanceError`] if the lock path can't be resolved, the lock file
/// can't be opened, another instance already holds the lock, or the lock
/// syscall itself fails.
pub fn acquire(lock_name: &str) -> Result<InstanceGuard, InstanceError> {
    let path = paths::config_dir()?.join(lock_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| InstanceError::Open {
            path: path.clone(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| InstanceError::Open {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => {
            debug!(path = %path.display(), "single-instance lock acquired");
            Ok(InstanceGuard { _handle: file })
        }
        Err(TryLockError::WouldBlock) => Err(InstanceError::AlreadyRunning { path }),
        Err(TryLockError::Error(source)) => Err(InstanceError::LockFailed { path, source }),
    }
}

//! Platforms without a clipboard backend yet: macOS, Linux, and the rest.
//! Flow does not advertise clipboard sync from them, so peers never send them
//! text or files and they never offer any.

use std::path::PathBuf;

use crate::{ClipboardError, ClipboardText};

pub(crate) const AVAILABLE: bool = false;

pub(crate) fn read_text(_max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
    Err(ClipboardError::Unsupported)
}

pub(crate) fn write_text(_text: &str) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unsupported)
}

pub(crate) fn read_files() -> Result<Option<Vec<PathBuf>>, ClipboardError> {
    Err(ClipboardError::Unsupported)
}

pub(crate) fn write_files(_paths: &[PathBuf]) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unsupported)
}

pub(crate) const fn change_count() -> Option<u64> {
    None
}

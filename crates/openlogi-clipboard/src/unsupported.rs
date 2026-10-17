//! Platforms without a clipboard backend yet: macOS, Linux, and the rest.
//! Flow does not advertise clipboard sync from them, so peers never send them
//! text and they never offer any.

use crate::{ClipboardError, ClipboardText};

pub(crate) const AVAILABLE: bool = false;

pub(crate) fn read_text(_max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
    Err(ClipboardError::Unsupported)
}

pub(crate) fn write_text(_text: &str) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unsupported)
}

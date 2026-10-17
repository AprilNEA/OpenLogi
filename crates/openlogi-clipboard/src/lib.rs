//! Plain-text access to the operating system clipboard, for Flow's clipboard
//! sync.
//!
//! | Platform | Implementation |
//! |----------|----------------|
//! | Windows  | `CF_UNICODETEXT` via `OpenClipboard` / `GetClipboardData` / `SetClipboardData` |
//! | macOS, Linux, others | none yet: [`available`] is `false` and every call fails with [`ClipboardError::Unsupported`] |
//!
//! Calls block — Windows retries briefly while another program holds the
//! clipboard open — so async callers run them on a blocking thread. Nothing
//! here logs, and neither [`ClipboardText`]'s `Debug` output nor any error
//! carries clipboard contents.

use std::fmt;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(target_os = "windows"))]
mod unsupported;
#[cfg(not(target_os = "windows"))]
use unsupported as platform;

/// What the clipboard holds as plain text.
#[derive(Clone, PartialEq, Eq)]
pub enum ClipboardText {
    /// No text: the clipboard is empty, holds empty text, or holds other
    /// formats only.
    Absent,
    /// The text, within the byte limit the caller passed.
    Text(String),
    /// Text longer than the byte limit the caller passed; it was not copied
    /// out.
    TooLarge,
}

impl fmt::Debug for ClipboardText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("Absent"),
            Self::Text(text) => formatter
                .debug_struct("Text")
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::TooLarge => formatter.write_str("TooLarge"),
        }
    }
}

/// Why the clipboard could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// This platform has no clipboard backend.
    #[error("clipboard access is not supported on this platform")]
    Unsupported,
    /// Another program kept the clipboard open through every retry.
    #[error("the clipboard is held open by another program")]
    Busy,
    /// The operating system refused a clipboard operation.
    #[error("clipboard {operation} failed: {source}")]
    Os {
        /// The step that failed, e.g. `"write"`.
        operation: &'static str,
        /// The OS error reported for it.
        #[source]
        source: std::io::Error,
    },
}

/// Whether this platform has a clipboard backend.
#[must_use]
pub const fn available() -> bool {
    platform::AVAILABLE
}

/// Reads the clipboard's plain text, copying it out only when it is at most
/// `max_bytes` long in UTF-8.
pub fn read_text(max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
    platform::read_text(max_bytes)
}

/// Replaces the clipboard's contents with `text`.
pub fn write_text(text: &str) -> Result<(), ClipboardError> {
    platform::write_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_carries_the_text() {
        let printed = format!("{:?}", ClipboardText::Text("hunter2".to_owned()));
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("bytes: 7"));
    }
}

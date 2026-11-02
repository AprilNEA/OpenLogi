//! The revision of `config.toml`, which tells a reader of a save from a
//! reader of a later edit.

use serde::{Deserialize, Serialize};

/// A digest of the exact text of a config file. Two readers of the same text
/// agree on it, and any edit of the text changes it.
///
/// The default revision is the revision of no file: it matches no file that
/// is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConfigRevision([u8; 32]);

impl ConfigRevision {
    /// The revision of a file that holds `text`: its SHA-256 digest.
    #[cfg(feature = "fs")]
    #[must_use]
    pub fn of(text: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self(Sha256::digest(text.as_bytes()).into())
    }
}

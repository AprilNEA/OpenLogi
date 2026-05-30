#![allow(
    dead_code,
    reason = "full schema parsed; only a subset is consumed by today's callers"
)]

//! Parses the `index.json` shipped by assets.openlogi.org.
//!
//! Schema mirrors the file the assets repo's `stage_assets.py` emits:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "devices": {
//!     "<depot>": {
//!       "modelId": "6b023",
//!       "displayName": "MX Master 3",
//!       "type": "MOUSE",
//!       "asset_path": "v1/devices/mx_master_3/",
//!       "files": [{ "name": "front_core.png", "sha256": "...", "bytes": 388329 }]
//!     }
//!   }
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::http;

#[derive(Debug, Deserialize)]
pub struct Index {
    pub schema_version: u32,
    pub devices: HashMap<String, DeviceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceEntry {
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub asset_path: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The files every depot must ship, fetched as the per-depot baseline by
/// both the CLI bundle sync and the GUI runtime sync:
///
/// - `core_metadata.json` — hotspot percentages for the buttons overlay
/// - `manifest.json` — `extended_model_id` → colour-variant + resource-key
///   filename lookup
/// - `front_core.png` — the carousel render (and the buttons render on
///   simpler devices whose manifest points `device_buttons_image` at it)
pub const CORE_FILES: [&str; 3] = ["core_metadata.json", "manifest.json", "front_core.png"];

impl Index {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        http::load_json(path)
    }

    /// Find the depot whose `modelId` matches `model_id` exactly.
    #[must_use]
    pub fn find_by_model_id(&self, model_id: &str) -> Option<(&str, &DeviceEntry)> {
        self.devices
            .iter()
            .find(|(_, entry)| entry.model_id.eq_ignore_ascii_case(model_id))
            .map(|(depot, entry)| (depot.as_str(), entry))
    }

    /// Find the depot whose `modelId` ends with `suffix` (case-insensitive).
    ///
    /// Used as a fallback when the strict `ext + bolt_pid` formatting
    /// doesn't line up — Logi's registry stores e.g. `"2b042"` for the
    /// MX Master 4 even though HID++ DeviceInformation reports `ext=01`
    /// on the same device. Matching on the trailing bolt PID is still
    /// unambiguous in practice because Logitech reserves PID ranges per
    /// product family.
    #[must_use]
    pub fn find_by_model_id_suffix(&self, suffix: &str) -> Option<(&str, &DeviceEntry)> {
        let suffix_lower = suffix.to_ascii_lowercase();
        self.devices
            .iter()
            .find(|(_, entry)| entry.model_id.to_ascii_lowercase().ends_with(&suffix_lower))
            .map(|(depot, entry)| (depot.as_str(), entry))
    }
}

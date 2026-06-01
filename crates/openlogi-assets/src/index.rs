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

    /// Find the depot whose `displayName` equals `name` (case-insensitive,
    /// exact). Last-resort bridge for devices whose live HID++ model PID is
    /// absent from the registry under every transport — e.g. an MX Master 3S
    /// connected over BTLE reports model id `b034`, but Logi's registry keys
    /// it `2b043` (a different transport's PID). The firmware codename
    /// ("MX Master 3S") still matches the registry `displayName`.
    #[must_use]
    pub fn find_by_display_name(&self, name: &str) -> Option<(&str, &DeviceEntry)> {
        self.devices
            .iter()
            .find(|(_, entry)| entry.display_name.eq_ignore_ascii_case(name))
            .map(|(depot, entry)| (depot.as_str(), entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(model_id: &str, display_name: &str) -> DeviceEntry {
        DeviceEntry {
            model_id: model_id.to_string(),
            display_name: display_name.to_string(),
            kind: "mouse".to_string(),
            asset_path: "assets/mx_master_3s/".to_string(),
            files: Vec::new(),
        }
    }

    fn index_with(depot: &str, model_id: &str, display_name: &str) -> Index {
        let mut devices = HashMap::new();
        devices.insert(depot.to_string(), entry(model_id, display_name));
        Index {
            schema_version: 1,
            devices,
        }
    }

    #[test]
    fn find_by_display_name_matches_case_insensitively() {
        let index = index_with("mx_master_3s", "2b043", "MX Master 3S");
        let hit = index.find_by_display_name("mx master 3s");
        assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
    }

    #[test]
    fn find_by_display_name_is_exact_not_substring() {
        // "MX Master 3" must not resolve to the "MX Master 3S" depot —
        // the bridge is an exact (case-insensitive) name match.
        let index = index_with("mx_master_3s", "2b043", "MX Master 3S");
        assert!(index.find_by_display_name("MX Master 3").is_none());
    }
}

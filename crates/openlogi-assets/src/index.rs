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
//!       "modelId": "2b043",
//!       "modelIds": ["2b043", "2b034"],
//!       "displayName": "MX Master 3S",
//!       "type": "MOUSE",
//!       "asset_path": "v1/devices/mx_master_3s/",
//!       "files": [{ "name": "front_core.png", "sha256": "...", "bytes": 388329 }]
//!     }
//!   }
//! }
//! ```
//!
//! A depot can answer to several model ids — a product's transports and
//! hardware revisions share one asset depot (the MX Master 3S reports bolt
//! pid `b034` over BTLE but `b043` via a Bolt receiver). `modelIds` carries
//! the full set; `modelId` stays as the primary for older clients. Index
//! files generated before `modelIds` existed simply omit it, so it defaults
//! to empty and lookups fall back to `modelId`.

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
    /// Every model id Logi lists for this depot — different transports or
    /// hardware revisions of the same product. Empty on index files that
    /// predate the field; [`model_id_candidates`](Self::model_id_candidates)
    /// then falls back to [`model_id`](Self::model_id).
    #[serde(rename = "modelIds", default)]
    pub model_ids: Vec<String>,
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

/// Filename schemas Logi ships, most-preferred first. Newer depots use the
/// `*_core` names; older ones — most keyboards, the MX Vertical, older mice —
/// ship the bare names. A depot commits to one schema, never a mix, so
/// resolving each slot to the first name the registry actually lists picks
/// the right one. The manifest then maps `device_image` /
/// `device_buttons_image` to the concrete render for colour variants.
pub const METADATA_FILES: [&str; 2] = ["core_metadata.json", "metadata.json"];
pub const FRONT_RENDER_FILES: [&str; 2] = ["front_core.png", "front.png"];
pub const BUTTONS_RENDER_FILES: [&str; 2] = ["side_core.png", "side.png"];

impl DeviceEntry {
    /// Every model id this depot answers to, primary first. Yields the
    /// canonical [`model_id`](Self::model_id) followed by any additional
    /// [`model_ids`](Self::model_ids) (deduplicated against the primary).
    /// On a legacy index without `modelIds` this is just the primary, so
    /// matching behaves exactly as it did before the field existed.
    pub fn model_id_candidates(&self) -> impl Iterator<Item = &str> + '_ {
        std::iter::once(self.model_id.as_str()).chain(
            self.model_ids
                .iter()
                .map(String::as_str)
                .filter(move |id| !id.eq_ignore_ascii_case(self.model_id.as_str())),
        )
    }

    /// First of `candidates` this depot's registry file list contains —
    /// the concrete filename for a schema slot (metadata / hero render /
    /// buttons render). `None` when the depot ships none of them.
    #[must_use]
    pub fn preferred_file(&self, candidates: &[&'static str]) -> Option<&'static str> {
        candidates
            .iter()
            .copied()
            .find(|name| self.files.iter().any(|f| f.name == *name))
    }

    /// Baseline files both syncs fetch per depot: hotspot metadata (either
    /// schema), the manifest, and the hero render (either schema). A slot
    /// the depot doesn't ship is skipped — a camera/receiver depot with no
    /// metadata or render contributes just the manifest, if even that.
    #[must_use]
    pub fn baseline_files(&self) -> Vec<&'static str> {
        let mut files = Vec::with_capacity(3);
        files.extend(self.preferred_file(&METADATA_FILES));
        if self.files.iter().any(|f| f.name == "manifest.json") {
            files.push("manifest.json");
        }
        files.extend(self.preferred_file(&FRONT_RENDER_FILES));
        files
    }
}

impl Index {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        http::load_json(path)
    }

    /// Find the depot one of whose model ids matches `model_id` exactly.
    #[must_use]
    pub fn find_by_model_id(&self, model_id: &str) -> Option<(&str, &DeviceEntry)> {
        self.devices
            .iter()
            .find(|(_, entry)| {
                entry
                    .model_id_candidates()
                    .any(|id| id.eq_ignore_ascii_case(model_id))
            })
            .map(|(depot, entry)| (depot.as_str(), entry))
    }

    /// Find the depot one of whose model ids ends with `suffix`
    /// (case-insensitive).
    ///
    /// Used as a fallback when the strict `ext + bolt_pid` formatting
    /// doesn't line up — Logi's registry stores e.g. `"2b042"` for the
    /// MX Master 4 even though HID++ DeviceInformation reports `ext=01`
    /// on the same device. Scanning every listed model id also catches a
    /// transport whose bolt pid differs from the depot's primary — the
    /// MX Master 3S reports `b034` over BTLE, listed alongside `b043`.
    /// Matching on the trailing bolt PID is still unambiguous in practice
    /// because Logitech reserves PID ranges per product family.
    #[must_use]
    pub fn find_by_model_id_suffix(&self, suffix: &str) -> Option<(&str, &DeviceEntry)> {
        let suffix_lower = suffix.to_ascii_lowercase();
        self.devices
            .iter()
            .find(|(_, entry)| {
                entry
                    .model_id_candidates()
                    .any(|id| id.to_ascii_lowercase().ends_with(&suffix_lower))
            })
            .map(|(depot, entry)| (depot.as_str(), entry))
    }

    /// Find the depot whose `displayName` equals `name` (case-insensitive,
    /// exact). Last-resort bridge for a device whose live HID++ model id
    /// matches none of the depot's listed `modelIds` — now mainly a legacy
    /// index that predates `modelIds` and so lists only one transport's pid
    /// (e.g. only `2b043` for an MX Master 3S that reports `b034` over BTLE).
    /// The firmware codename ("MX Master 3S") still matches the `displayName`.
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
            model_ids: Vec::new(),
            display_name: display_name.to_string(),
            kind: "mouse".to_string(),
            asset_path: "assets/mx_master_3s/".to_string(),
            files: Vec::new(),
        }
    }

    fn index_with(depot: &str, model_id: &str, display_name: &str) -> Index {
        index_of(depot, entry(model_id, display_name))
    }

    fn index_of(depot: &str, entry: DeviceEntry) -> Index {
        let mut devices = HashMap::new();
        devices.insert(depot.to_string(), entry);
        Index {
            schema_version: 1,
            devices,
        }
    }

    #[test]
    fn model_id_candidates_falls_back_to_primary_for_legacy_entry() {
        // No `modelIds` (old index) → matching runs off the lone `modelId`.
        let e = entry("2b043", "MX Master 3S");
        assert_eq!(e.model_id_candidates().collect::<Vec<_>>(), ["2b043"]);
    }

    #[test]
    fn model_id_candidates_lists_primary_then_extras_without_dupes() {
        let mut e = entry("2b043", "MX Master 3S");
        e.model_ids = vec!["2b043".into(), "2b034".into()];
        assert_eq!(
            e.model_id_candidates().collect::<Vec<_>>(),
            ["2b043", "2b034"]
        );
    }

    #[test]
    fn find_by_model_id_matches_any_listed_id() {
        let mut e = entry("2b043", "MX Master 3S");
        e.model_ids = vec!["2b043".into(), "2b034".into()];
        let index = index_of("mx_master_3s", e);
        // Both the primary and the secondary id resolve to the depot.
        assert_eq!(
            index.find_by_model_id("2b034").map(|(d, _)| d),
            Some("mx_master_3s")
        );
        assert_eq!(
            index.find_by_model_id("2b043").map(|(d, _)| d),
            Some("mx_master_3s")
        );
    }

    #[test]
    fn find_by_model_id_suffix_matches_secondary_id() {
        // The BTLE MX Master 3S reports bolt pid `b034`; listing it next to
        // `b043` lets the suffix match resolve the depot by pid alone.
        let mut e = entry("2b043", "MX Master 3S");
        e.model_ids = vec!["2b043".into(), "2b034".into()];
        let index = index_of("mx_master_3s", e);
        assert_eq!(
            index.find_by_model_id_suffix("b034").map(|(d, _)| d),
            Some("mx_master_3s")
        );
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

    fn entry_with_files(names: &[&str]) -> DeviceEntry {
        let mut e = entry("2b043", "MX Master 3S");
        e.files = names
            .iter()
            .map(|name| FileEntry {
                name: (*name).to_string(),
                sha256: String::new(),
                bytes: 0,
            })
            .collect();
        e
    }

    #[test]
    fn baseline_files_resolves_core_schema() {
        let e = entry_with_files(&["core_metadata.json", "manifest.json", "front_core.png"]);
        assert_eq!(
            e.baseline_files(),
            ["core_metadata.json", "manifest.json", "front_core.png"]
        );
    }

    #[test]
    fn baseline_files_resolves_old_schema() {
        // MX Vertical / most keyboards ship the bare names — the same slots
        // resolve to `metadata.json` + `front.png`.
        let e = entry_with_files(&["metadata.json", "manifest.json", "front.png", "side.png"]);
        assert_eq!(
            e.baseline_files(),
            ["metadata.json", "manifest.json", "front.png"]
        );
        assert_eq!(e.preferred_file(&BUTTONS_RENDER_FILES), Some("side.png"));
    }

    #[test]
    fn baseline_files_skips_missing_slots() {
        // A depot with no hotspot metadata or render (camera/receiver)
        // contributes only the manifest.
        let e = entry_with_files(&["manifest.json"]);
        assert_eq!(e.baseline_files(), ["manifest.json"]);
        assert_eq!(e.preferred_file(&METADATA_FILES), None);
    }
}

//! Parses the per-depot `manifest.json` from a Logi Options+ payload.
//!
//! Each depot ships one entry per color / SKU variant. The base entry's
//! `modelId` matches the HID++ device's bolt PID (e.g. `"2b042"` for an
//! MX Master 4); colour variants append `_extN` matching the device's
//! [`extended_model_id`](https://en.wikipedia.org/wiki/USB_human_interface_device_class)
//! byte (so `_ext1`, `_ext2`, …).
//!
//! Schema (observed-from-the-wild):
//!
//! ```json
//! {
//!   "devices": [
//!     {
//!       "modelId": "2b042",
//!       "resources": [
//!         { "key": "device_image", "src": "front_core.png" },
//!         { "key": "device_buttons_image", "src": "side_core.png" }
//!       ]
//!     },
//!     {
//!       "modelId": "2b042_ext1",
//!       "resources": [
//!         { "key": "device_image", "src": "front_ext_1.png" }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Only `device_image` is consumed today — that's the PNG the GUI
//! renders. The rest of the schema is parsed permissively so additional
//! fields don't break older clients.

use std::path::Path;

use serde::Deserialize;

use crate::error::AssetError;
use crate::http;

/// Top-level `manifest.json` document.
///
/// Supports both the multi-variant `{"devices": [...]}` schema and the
/// single-depot flat `{"resources": [...]}` schema (as seen in the K380 depot).
#[derive(Debug, Deserialize)]
pub struct DepotManifest {
    #[serde(default)]
    pub devices: Vec<ManifestDevice>,
    #[serde(default)]
    pub resources: Vec<ManifestResource>,
}

/// One device variant — base model or a colour SKU.
#[derive(Debug, Deserialize)]
pub struct ManifestDevice {
    #[serde(rename = "modelId")]
    pub model_id: String,
    pub resources: Vec<ManifestResource>,
}

/// One (`key`, `src`) pair. `key` is a stable Logitech identifier
/// (`device_image`, `device_buttons_image`, …); `src` is a filename
/// relative to the depot directory.
#[derive(Debug, Deserialize)]
pub struct ManifestResource {
    pub key: String,
    pub src: String,
}

impl DepotManifest {
    /// Load and parse a `manifest.json` from disk.
    pub fn load_from(path: &Path) -> Result<Self, AssetError> {
        http::load_json(path)
    }

    /// Returns the `device_image` filename for the variant matching
    /// `model_id` (case-insensitive). `None` when the manifest doesn't
    /// know that variant — callers fall back to `front_core.png`.
    #[must_use]
    pub fn device_image_for(&self, model_id: &str) -> Option<&str> {
        self.resource_for(model_id, "device_image")
    }

    /// Returns the `src` filename of the manifest resource whose `key`
    /// equals `resource_key` (e.g. `"device_buttons_image"`) for the
    /// variant matching `model_id`. Case-insensitive on the model id.
    #[must_use]
    pub fn resource_for(&self, model_id: &str, resource_key: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.model_id.eq_ignore_ascii_case(model_id))
            .and_then(|d| d.resources.iter().find(|r| r.key == resource_key))
            .map(|r| r.src.as_str())
            .or_else(|| {
                self.resources
                    .iter()
                    .find(|r| r.key == resource_key)
                    .map(|r| r.src.as_str())
            })
    }

    /// Resolve `resource_key`'s `src` filename for the colour variant
    /// identified by `ext` (`0` = the base model). Combines
    /// [`variant_model_id`] with [`Self::resource_for`] — the
    /// `(base_model_id, ext, key)` → filename lookup the GUI does at both
    /// download and render time.
    #[must_use]
    pub fn resource_for_variant(
        &self,
        base_model_id: &str,
        ext: u8,
        resource_key: &str,
    ) -> Option<&str> {
        self.resource_for(&variant_model_id(base_model_id, ext), resource_key)
    }
}

/// Build the variant model-id string a HID++ device should match
/// against the depot manifest.
///
/// - `ext == 0` → the bare base model id.
/// - `ext == N` → `"{base}_ext{N}"`.
///
/// Logitech's stable convention; documented separately so future
/// firmware quirks (different separators, lossy encoding) only need
/// one site updated.
#[must_use]
pub fn variant_model_id(base: &str, ext: u8) -> String {
    if ext == 0 {
        base.to_string()
    } else {
        format!("{base}_ext{ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_variant_manifest() {
        let json = r#"{
            "devices": [
                {
                    "modelId": "2b042",
                    "resources": [
                        { "key": "device_image", "src": "front_core.png" }
                    ]
                },
                {
                    "modelId": "2b042_ext1",
                    "resources": [
                        { "key": "device_image", "src": "front_ext1.png" }
                    ]
                }
            ]
        }"#;

        let manifest: DepotManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.device_image_for("2b042"), Some("front_core.png"));
        assert_eq!(
            manifest.resource_for_variant("2b042", 1, "device_image"),
            Some("front_ext1.png")
        );
        assert_eq!(manifest.device_image_for("unknown"), None);
    }

    #[test]
    fn parses_flat_resources_manifest() {
        let json = r#"{
            "resources": [
                { "key": "manifest", "src": "manifest.json" },
                { "key": "device_image", "src": "front.png" },
                { "key": "image_metadata", "src": "metadata.json" }
            ]
        }"#;

        let manifest: DepotManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.device_image_for("1b342"), Some("front.png"));
        assert_eq!(
            manifest.resource_for_variant("1b342", 0, "device_image"),
            Some("front.png")
        );
        assert_eq!(
            manifest.resource_for_variant("1b342", 0, "image_metadata"),
            Some("metadata.json")
        );
        assert_eq!(manifest.resource_for("any_id", "non_existent"), None);
    }
}

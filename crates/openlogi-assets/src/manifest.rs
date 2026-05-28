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
//! Only `device_image` is consumed today — that's the PNG `AssetCache`
//! renders. The rest of the schema is parsed permissively so additional
//! fields don't break older clients.

#![allow(
    dead_code,
    reason = "schema captured in full; only `device_image` is consumed in v0.0.1"
)]

use std::path::Path;

use serde::Deserialize;

use crate::http;

/// Top-level `manifest.json` document.
#[derive(Debug, Deserialize)]
pub struct DepotManifest {
    pub devices: Vec<ManifestDevice>,
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
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        http::load_json(path)
    }

    /// Returns the `device_image` filename for the variant matching
    /// `model_id` (case-insensitive). `None` when the manifest doesn't
    /// know that variant — callers fall back to `front_core.png`.
    #[must_use]
    pub fn device_image_for(&self, model_id: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.model_id.eq_ignore_ascii_case(model_id))
            .and_then(|d| d.resources.iter().find(|r| r.key == "device_image"))
            .map(|r| r.src.as_str())
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

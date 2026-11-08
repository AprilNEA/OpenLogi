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
//! OpenLogi consumes five resources: the front, buttons, side and camera
//! renders and the hotspot metadata. [`DepotManifest::consumed_srcs_for`]
//! names the files one variant needs, [`DepotManifest::consumed_srcs`] the
//! files every variant needs. The rest of the schema is parsed permissively
//! so additional fields don't break older clients.

use std::path::Path;

use serde::Deserialize;

use crate::error::AssetError;
use crate::http;

/// The resource keys whose `src` OpenLogi draws or reads. The per-device
/// sync downloads these for the connected variant and the offline bundle
/// ships them for every variant, so both ask [`DepotManifest`] instead of
/// listing keys themselves.
const CONSUMED_KEYS: [&str; 5] = [
    "device_image",
    "device_buttons_image",
    "device_side",
    "device_camera_image",
    "image_metadata",
];

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
/// (`device_image`, `device_buttons_image`, …); `src` is a path relative to
/// the depot directory — usually a bare filename, but G HUB depots point
/// into sub-directories (`images/device/<uuid>`). Join it with
/// [`http::safe_relative_path`].
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

    /// The files one colour variant draws or reads: for each consumed
    /// resource, the `src` of the first base in `bases` whose `ext` variant
    /// names it. `bases` is every model id the depot answers to, in priority
    /// order, because a manifest is keyed on whichever id Logi authored it
    /// against. Empty when no base knows the variant.
    #[must_use]
    pub fn consumed_srcs_for<S: AsRef<str>>(&self, bases: &[S], ext: u8) -> Vec<&str> {
        let mut srcs = Vec::new();
        for key in CONSUMED_KEYS {
            let src = bases
                .iter()
                .find_map(|base| self.resource_for_variant(base.as_ref(), ext, key));
            if let Some(src) = src
                && !srcs.contains(&src)
            {
                srcs.push(src);
            }
        }
        srcs
    }

    /// Every file any variant of this depot draws or reads, once each, in
    /// manifest order. An offline bundle has to carry all of them to draw
    /// every variant without the network.
    #[must_use]
    pub fn consumed_srcs(&self) -> Vec<&str> {
        let mut srcs = Vec::new();
        for resource in self.devices.iter().flat_map(|d| &d.resources) {
            let src = resource.src.as_str();
            if CONSUMED_KEYS.contains(&resource.key.as_str()) && !srcs.contains(&src) {
                srcs.push(src);
            }
        }
        srcs
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
    use super::DepotManifest;

    /// A G HUB depot in the G502 X PLUS's shape: the base keeps its front at
    /// the depot root, every other render sits under `images/device/`, and
    /// the manifest also names files OpenLogi never draws.
    fn g_hub_manifest() -> DepotManifest {
        serde_json::from_str(
            r#"{"devices":[
                {"modelId":"g502x_plus","resources":[
                    {"key":"device_image","src":"front.png"},
                    {"key":"device_side","src":"images/device/base-side"},
                    {"key":"onboarding_data","src":"images/device/onboarding"}]},
                {"modelId":"g502x_plus_ext1","resources":[
                    {"key":"device_image","src":"images/device/white-front"},
                    {"key":"device_side","src":"images/device/white-side"},
                    {"key":"image_metadata","src":"metadata.json"}]},
                {"modelId":"g502x_plus_ext2","resources":[
                    {"key":"device_image","src":"images/device/white-front"}]}
            ]}"#,
        )
        .expect("manifest parses")
    }

    #[test]
    fn every_variant_names_its_consumed_files_once_and_skips_the_rest() {
        assert_eq!(
            g_hub_manifest().consumed_srcs(),
            [
                "front.png",
                "images/device/base-side",
                "images/device/white-front",
                "images/device/white-side",
                "metadata.json",
            ]
        );
    }

    #[test]
    fn one_variant_names_only_its_own_consumed_files() {
        let manifest = g_hub_manifest();
        assert_eq!(
            manifest.consumed_srcs_for(&["c095", "g502x_plus"], 1),
            [
                "images/device/white-front",
                "images/device/white-side",
                "metadata.json",
            ]
        );
        assert_eq!(
            manifest.consumed_srcs_for(&["g502x_plus"], 0),
            ["front.png", "images/device/base-side"]
        );
    }

    #[test]
    fn the_first_base_naming_a_resource_wins_for_that_resource() {
        let manifest: DepotManifest = serde_json::from_str(
            r#"{"devices":[
                {"modelId":"4099_ext1","resources":[
                    {"key":"device_image","src":"front_ext1.png"}]},
                {"modelId":"c095_ext1","resources":[
                    {"key":"device_image","src":"front_ext_1.png"},
                    {"key":"device_side","src":"side_ext_1.png"}]}
            ]}"#,
        )
        .expect("manifest parses");
        assert_eq!(
            manifest.consumed_srcs_for(&["4099", "c095"], 1),
            ["front_ext1.png", "side_ext_1.png"]
        );
    }

    #[test]
    fn a_variant_no_base_knows_needs_no_files() {
        let manifest = g_hub_manifest();
        assert!(manifest.consumed_srcs_for(&["c095"], 1).is_empty());
        assert!(manifest.consumed_srcs_for(&["g502x_plus"], 9).is_empty());
        assert!(manifest.consumed_srcs_for::<&str>(&[], 0).is_empty());
    }
}

//! Parses the per-depot hotspot metadata shipped by the Logi Options+
//! installer (and re-hosted by assets.openlogi.org) — `core_metadata.json`
//! on newer depots, `metadata.json` on older ones. The caller picks the
//! filename and hands the path to [`Metadata::load_from`].
//!
//! The two generations *mostly* share a schema, but older `metadata.json`
//! files (e.g. the G513 keyboard depot) identify assignments by `slotId`
//! only — there is no `slotName` — so every observed-optional field must
//! stay soft: one missing field would otherwise fail the whole file and
//! drop the `origin` dimensions the renderer needs.
//!
//! Only the fields OpenLogi actually consumes are deserialized — every
//! other field is silently ignored. The schema below is observed-from-the-
//! wild, not derived from any Logitech specification.
//!
//! ```json
//! {
//!   "images": [
//!     {
//!       "key": "device_image",
//!       "origin": { "width": 687, "height": 1024 }
//!     },
//!     {
//!       "key": "device_buttons_image",
//!       "origin": { "width": 687, "height": 1024 },
//!       "assignments": [
//!         { "slotId": "...", "slotName": "SLOT_NAME_MIDDLE_BUTTON",
//!           "marker": { "x": 73, "y": 18 },
//!           "label":  { "x": 1,  "y": 0  } }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! `marker.{x,y}` is a percentage 0..100 of the device image's origin
//! dimensions. `label.{x,y}` is a direction code (-1 = left, 0 = centre,
//! +1 = right; same for y) hinting where the annotation card should sit
//! relative to the marker.

use std::path::Path;

use serde::Deserialize;

use crate::error::AssetError;
use crate::http;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Metadata {
    pub images: Vec<ImageEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ImageEntry {
    pub key: String,
    pub origin: Origin,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Assignment {
    /// Empty on older keyboard depots whose assignments carry only `slotId`;
    /// `map_slot_name`-style consumers treat unknown names as "no hotspot".
    #[serde(rename = "slotName", default)]
    pub slot_name: String,
    /// G-series depots (G502) identify slots by `slotId` only, e.g.
    /// `g502core_g7_m1`. Empty when the depot uses `slotName`.
    #[serde(rename = "slotId", default)]
    pub slot_id: String,
    /// Camera depots ship marker-less settings-slot assignments (under the
    /// `device_camera_image` entry, which no hotspot consumer reads); a
    /// missing marker defaults to the origin rather than failing the file.
    #[serde(default)]
    pub marker: Point,
    #[serde(default)]
    pub label: Direction,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
pub struct Direction {
    pub x: i32,
    pub y: i32,
}

impl Metadata {
    /// Load and parse a metadata JSON file from disk.
    pub fn load_from(path: &Path) -> Result<Self, AssetError> {
        http::load_json(path)
    }

    /// Image dimensions (use the `device_image` entry — both entries
    /// always share the same origin in practice).
    #[must_use]
    pub fn origin(&self) -> Option<Origin> {
        self.images.first().map(|i| i.origin)
    }

    /// Raw assignment iterator over the images Logi calibrates hotspots
    /// against. MX-class depots put them on `device_buttons_image`; G-series
    /// depots put them on `device_image` and `device_side` (no buttons
    /// image). Camera settings slots stay out.
    pub fn assignments(&self) -> impl Iterator<Item = &Assignment> + '_ {
        self.hotspot_images().flat_map(|img| img.assignments.iter())
    }

    /// Assignments authored against a single metadata image key.
    pub fn assignments_on<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Assignment> + 'a {
        self.images
            .iter()
            .filter(move |img| img.key == key)
            .flat_map(|img| img.assignments.iter())
    }

    /// Image entries that carry remappable-button markers.
    pub fn hotspot_images(&self) -> impl Iterator<Item = &ImageEntry> + '_ {
        let has_buttons_image = self
            .images
            .iter()
            .any(|img| img.key == "device_buttons_image" && !img.assignments.is_empty());
        self.images.iter().filter(move |img| {
            if img.assignments.is_empty() {
                return false;
            }
            if has_buttons_image {
                img.key == "device_buttons_image"
            } else {
                matches!(img.key.as_str(), "device_image" | "device_side")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Metadata;

    /// Older keyboard depots (G513) identify assignments by `slotId` only —
    /// no `slotName` — and add fields like `assignmentOffset`. Parsing must
    /// not fail wholesale: the renderer still needs `origin`, and unknown
    /// slot names already degrade to "no hotspot" in the consumer.
    #[test]
    fn old_slot_id_only_metadata_parses() {
        let json = r#"{
          "images": [
            {
              "key": "device_image",
              "origin": { "width": 3598, "height": 1315 },
              "assignmentOffset": { "x": 800, "y": 0 },
              "assignments": [
                { "slotId": "g513_g1_m1",
                  "marker": { "x": 370, "y": 300 },
                  "label":  { "x": -1200, "y": 300 } }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("old schema must parse");
        let origin = meta.origin().expect("origin survives");
        assert_eq!((origin.width, origin.height), (3598, 1315));
        assert_eq!(meta.images[0].assignments[0].slot_name, "");
        assert_eq!(meta.images[0].assignments[0].slot_id, "g513_g1_m1");
        assert_eq!(meta.assignments().count(), 1);
    }

    /// Camera depots (StreamCam) list settings-slot assignments with no
    /// `marker` under their `device_camera_image` entry. Parsing must not
    /// fail wholesale, and `assignments()` must not surface them (it reads
    /// only the `device_buttons_image` entry).
    #[test]
    fn camera_metadata_without_markers_parses() {
        let json = r#"{
          "images": [
            { "key": "device_image", "origin": { "width": 1280, "height": 800 } },
            {
              "key": "device_camera_image",
              "origin": { "width": 396, "height": 396 },
              "assignments": [
                { "slotId": "streamcam-0893_webcam_camera_settings",
                  "slotName": "SLOT_NAME_WEBCAM_CAMERA_SETTINGS",
                  "disableAssignmentClick": true }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("camera schema must parse");
        let origin = meta.origin().expect("origin survives");
        assert_eq!((origin.width, origin.height), (1280, 800));
        assert_eq!(meta.assignments().count(), 0);
    }

    #[test]
    fn g502_slot_id_assignments_on_front_and_side_are_visible() {
        let json = r#"{
          "images": [
            {
              "key": "device_image",
              "origin": { "width": 1391, "height": 2700 },
              "assignments": [
                { "slotId": "g502core_g3_m1", "marker": { "x": 800, "y": 869 } }
              ]
            },
            {
              "key": "device_side",
              "origin": { "width": 936, "height": 2700 },
              "assignments": [
                { "slotId": "g502core_g4_m1", "marker": { "x": 580, "y": 1800 } }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("g502 schema must parse");
        assert_eq!(meta.assignments().count(), 2);
        assert_eq!(meta.assignments_on("device_side").count(), 1);
    }
}

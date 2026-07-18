#![allow(
    dead_code,
    reason = "full schema parsed; label direction codes + extra coords land in later phases"
)]

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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metadata {
    pub images: Vec<ImageEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageEntry {
    pub key: String,
    pub origin: Origin,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct Origin {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Assignment {
    /// Empty on older keyboard depots whose assignments carry only `slotId`;
    /// `map_slot_name`-style consumers treat unknown names as "no hotspot".
    #[serde(rename = "slotName", default)]
    pub slot_name: String,
    pub marker: Point,
    #[serde(default)]
    pub label: Direction,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Deserialize, Clone, Copy, Default)]
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

    /// Raw assignment iterator over the `device_buttons_image` entry.
    /// Slot-name → application-button mapping is intentionally left to
    /// the consumer (the GUI owns the ButtonId enum).
    pub fn assignments(&self) -> impl Iterator<Item = &Assignment> + '_ {
        self.images
            .iter()
            .find(|i| i.key == "device_buttons_image")
            .into_iter()
            .flat_map(|img| img.assignments.iter())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
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
    }
}

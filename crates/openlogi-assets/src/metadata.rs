#![allow(
    dead_code,
    reason = "full schema parsed; label direction codes + extra coords land in later phases"
)]

//! Parses the per-depot `core_metadata.json` shipped by the Logi Options+
//! installer (and re-hosted by assets.openlogi.org).
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

use crate::http;

#[derive(Debug, Clone, Deserialize)]
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
    #[serde(rename = "slotName")]
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
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
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

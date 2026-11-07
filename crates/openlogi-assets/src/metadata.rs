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
    /// Lighting zones drawn on this image. G HUB-style keyboard depots ship a
    /// `PERKEY_KEYBOARD` zone whose components are the keys, see
    /// [`Metadata::key_regions`]. Zones in a shape this parser does not
    /// expect are dropped rather than failing the file, since the hotspots
    /// and `origin` beside them still render.
    #[serde(default, deserialize_with = "lenient_zones")]
    pub zones: Vec<Zone>,
}

/// One lighting zone of an image entry and the parts it lights.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Zone {
    pub id: String,
    #[serde(default)]
    pub components: Vec<ZoneComponent>,
}

/// One lit part of a [`Zone`]. In a `PERKEY_KEYBOARD` zone, `id` is the
/// key's USB HID keyboard usage (`41` is Esc, `224` left Ctrl).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ZoneComponent {
    pub id: u16,
    #[serde(default)]
    pub renderings: Vec<Rendering>,
}

/// How a zone component is drawn on the image, in image pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Rendering {
    Rect {
        rect: Rect,
    },
    /// An SVG path, such as an ISO Enter key's L shape.
    Path {
        d: String,
    },
    /// A shape this parser does not know, kept so the file still loads.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Where one key sits on a device image, from a `PERKEY_KEYBOARD` zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyRegion {
    /// The key's USB HID keyboard usage.
    pub usage: u8,
    /// The key's box in image pixels. A path-drawn key (an ISO Enter) gets
    /// the box around its outline.
    pub rect: Rect,
    /// Whether the box is a path's outline rather than the key's own shape,
    /// so a hit test should prefer an overlapping plain key.
    pub outline: bool,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Assignment {
    /// Logi's per-depot slot identifier, e.g. `mx-keys-mini-2b369_c266`. On
    /// keyboard depots the `_c<decimal>` suffix is the key's HID++ `0x1b04`
    /// control ID — see [`Assignment::control_id`]. Empty on depots that
    /// predate the field.
    #[serde(rename = "slotId", default)]
    pub slot_id: String,
    /// Empty on older keyboard depots whose assignments carry only `slotId`;
    /// `map_slot_name`-style consumers treat unknown names as "no hotspot".
    #[serde(rename = "slotName", default)]
    pub slot_name: String,
    /// Camera depots ship marker-less settings-slot assignments (under the
    /// `device_camera_image` entry, which no hotspot consumer reads); a
    /// missing marker defaults to the origin rather than failing the file.
    #[serde(default)]
    pub marker: Point,
    #[serde(default)]
    pub label: Direction,
}

impl Assignment {
    /// The HID++ `0x1b04` control ID Logi's slot identifier encodes: the
    /// decimal after the trailing `_c`, as in `ergo-k860-6b359_c111` for the
    /// Lock key (`0x006f`). `None` for settings slots
    /// (`…_touchpad_settings`) and any depot that names slots differently.
    #[must_use]
    pub fn control_id(&self) -> Option<u16> {
        let (_, cid) = self.slot_id.rsplit_once("_c")?;
        cid.parse().ok()
    }

    /// The G-number in a G-series slot id (`g502wireless_g7_m1` → 7). These
    /// depots place markers in pixels, not percent.
    #[must_use]
    pub fn g_number(&self) -> Option<u8> {
        let (rest, mode) = self.slot_id.rsplit_once('_')?;
        mode.strip_prefix('m')?.parse::<u8>().ok()?;
        rest.rsplit_once("_g")?.1.parse().ok()
    }

    /// The keypad grid key Logi's slot identifier encodes: the row-major
    /// wire index after the trailing `_g`, as in `mx-keys-c354_g5` for the
    /// middle key of the 3×3 grid (1 = top-left). Grid keys report through
    /// the vendor report, not `0x1b04` controls, so no Logitech depot
    /// carries this suffix — only OpenLogi-authored depots do. Which
    /// indices exist is the keypad model's decision, so the decode returns
    /// the raw number and the consumer validates it.
    #[must_use]
    pub fn grid_index(&self) -> Option<u8> {
        let (_, index) = self.slot_id.rsplit_once("_g")?;
        index.parse().ok()
    }
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

    /// The image entry for `key`.
    #[must_use]
    pub fn image(&self, key: &str) -> Option<&ImageEntry> {
        self.images.iter().find(|i| i.key == key)
    }

    /// The keys a G HUB-style depot draws on its `device_image`: every
    /// component of the `PERKEY_KEYBOARD` zone whose id is a keyboard usage
    /// and whose shape is a rect or a path. Empty for depots without one.
    #[must_use]
    pub fn key_regions(&self) -> Vec<KeyRegion> {
        let Some(zone) = self
            .images
            .iter()
            .filter(|image| image.key == "device_image")
            .flat_map(|image| image.zones.iter())
            .find(|zone| zone.id == "PERKEY_KEYBOARD")
        else {
            return Vec::new();
        };
        zone.components
            .iter()
            .filter_map(|component| {
                let usage = u8::try_from(component.id).ok()?;
                component
                    .renderings
                    .iter()
                    .find_map(|rendering| match rendering {
                        Rendering::Rect { rect } => Some(KeyRegion {
                            usage,
                            rect: *rect,
                            outline: false,
                        }),
                        Rendering::Path { d } => path_bounds(d).map(|rect| KeyRegion {
                            usage,
                            rect,
                            outline: true,
                        }),
                        Rendering::Other => None,
                    })
            })
            .collect()
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

fn lenient_zones<'de, D>(deserializer: D) -> Result<Vec<Zone>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|error| {
        tracing::debug!(%error, "ignoring depot lighting zones in an unexpected shape");
        Vec::new()
    }))
}

/// The box around every point of an SVG path: its end points and, for
/// curves, its control points, which contain the curve. Supports the
/// `M L H V C S Q T Z` commands in absolute and relative form; `None` when
/// the path has no point or uses a command outside that set (an arc).
fn path_bounds(d: &str) -> Option<Rect> {
    let mut tokens = PathTokens { rest: d };
    let (mut x, mut y) = (0f32, 0f32);
    let (mut start_x, mut start_y) = (0f32, 0f32);
    let mut points: Vec<(f32, f32)> = Vec::new();
    while !tokens.at_end() {
        let mut cmd = tokens.command()?;
        // A command letter is followed by one or more argument groups.
        loop {
            let (dx, dy) = if cmd.is_ascii_lowercase() {
                (x, y)
            } else {
                (0., 0.)
            };
            match cmd.to_ascii_uppercase() {
                'Z' => {
                    (x, y) = (start_x, start_y);
                    break;
                }
                'M' | 'L' | 'T' => {
                    (x, y) = (tokens.number()? + dx, tokens.number()? + dy);
                    if cmd.eq_ignore_ascii_case(&'M') {
                        (start_x, start_y) = (x, y);
                        // Further pairs after a move are line-tos.
                        cmd = if cmd == 'M' { 'L' } else { 'l' };
                    }
                    points.push((x, y));
                }
                'H' => {
                    x = tokens.number()? + dx;
                    points.push((x, y));
                }
                'V' => {
                    y = tokens.number()? + dy;
                    points.push((x, y));
                }
                upper @ ('C' | 'S' | 'Q') => {
                    // Every point of a relative curve is relative to the
                    // segment's start; the last one is its end.
                    let count = if upper == 'C' { 3 } else { 2 };
                    for _ in 0..count {
                        let point = (tokens.number()? + dx, tokens.number()? + dy);
                        points.push(point);
                        (x, y) = point;
                    }
                }
                _ => return None,
            }
            if !tokens.next_is_number() {
                break;
            }
        }
    }
    let (first, rest) = points.split_first()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
    for &(px, py) in rest {
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    Some(Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

/// Splits an SVG path into command letters and numbers. Numbers may run
/// together (`1-2`, `.5.5`) and use exponents (`10e-6`).
struct PathTokens<'a> {
    rest: &'a str,
}

impl PathTokens<'_> {
    fn skip_separators(&mut self) {
        self.rest = self
            .rest
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }

    fn at_end(&mut self) -> bool {
        self.skip_separators();
        self.rest.is_empty()
    }

    fn next_is_number(&mut self) -> bool {
        self.skip_separators();
        self.rest
            .starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
    }

    fn command(&mut self) -> Option<char> {
        self.skip_separators();
        let c = self.rest.chars().next().filter(char::is_ascii_alphabetic)?;
        self.rest = &self.rest[1..];
        Some(c)
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let bytes = self.rest.as_bytes();
        let mut end = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
        let mut seen_dot = false;
        while let Some(&b) = bytes.get(end) {
            match b {
                b'0'..=b'9' => end += 1,
                b'.' if !seen_dot => {
                    seen_dot = true;
                    end += 1;
                }
                b'e' | b'E' => {
                    end += 1;
                    if matches!(bytes.get(end), Some(b'-' | b'+')) {
                        end += 1;
                    }
                    while matches!(bytes.get(end), Some(b'0'..=b'9')) {
                        end += 1;
                    }
                    break;
                }
                _ => break,
            }
        }
        let number = self.rest[..end].parse().ok()?;
        self.rest = &self.rest[end..];
        Some(number)
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
    fn gaming_slot_ids_decode_their_g_number() {
        let slot = |id: &str| super::Assignment {
            slot_id: id.into(),
            slot_name: String::new(),
            marker: super::Point::default(),
            label: super::Direction::default(),
        };
        assert_eq!(slot("g502wireless_g7_m1").g_number(), Some(7));
        assert_eq!(slot("g502wireless_g11_m1").g_number(), Some(11));
        assert_eq!(slot("g513_g1_m1").g_number(), Some(1));
        assert_eq!(slot("mx-keys-mini-2b369_c266").g_number(), None);
        assert_eq!(slot("g502wireless_g7").g_number(), None);
    }

    #[test]
    fn slot_ids_decode_their_control_id() {
        let json = r#"{
          "images": [
            {
              "key": "device_keys_image",
              "origin": { "width": 1872, "height": 728 },
              "assignments": [
                { "slotId": "mx-keys-mini-2b369_c266", "slotName": "SLOT_NAME_SCREEN_CAPTURE",
                  "marker": { "x": 53.5, "y": 13.8 } },
                { "slotId": "k400-4404d_touchpad_settings", "slotName": "SLOT_NAME_ZOOM_GESTURE",
                  "marker": { "x": 79, "y": 48 } },
                { "slotId": "g513_g1_m1", "marker": { "x": 370, "y": 300 } }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("parses");
        let cids: Vec<Option<u16>> = meta.images[0]
            .assignments
            .iter()
            .map(super::Assignment::control_id)
            .collect();
        assert_eq!(cids, vec![Some(0x010a), None, None]);
    }

    /// The `_g<index>` suffix OpenLogi-authored depots use for keypad grid
    /// keys. The legacy G513 macro slot name also contains `_g` but never
    /// decodes: the suffix must end the slot ID, not sit in its middle.
    #[test]
    fn slot_ids_decode_their_grid_index() {
        let json = r#"{
          "images": [
            {
              "key": "device_keys_image",
              "origin": { "width": 935, "height": 1105 },
              "assignments": [
                { "slotId": "mx-creative-keypad-c354_g1", "marker": { "x": 24.87, "y": 20.77 } },
                { "slotId": "mx-creative-keypad-c354_g9", "marker": { "x": 75.73, "y": 63.84 } },
                { "slotId": "mx-creative-keypad-c354_c417", "marker": { "x": 25.2, "y": 82.0 } },
                { "slotId": "g513_g1_m1", "marker": { "x": 370, "y": 300 } }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("parses");
        let indices: Vec<Option<u8>> = meta.images[0]
            .assignments
            .iter()
            .map(super::Assignment::grid_index)
            .collect();
        assert_eq!(indices, vec![Some(1), Some(9), None, None]);
    }

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

    /// A trimmed G515 LS TKL depot: Esc as a rect, ISO Enter as an L-shaped
    /// path, a circle and an unknown shape that must not break parsing.
    #[test]
    fn per_key_zone_yields_key_regions() {
        let json = r#"{"images":[{"key":"device_image","origin":{"width":2048,"height":845},
            "zones":[{"id":"PERKEY_KEYBOARD","components":[
                {"id":41,"renderings":[{"type":"rect","rect":{"x":10,"y":20,"width":30,"height":40}}]},
                {"id":40,"renderings":[{"type":"path","d":"m100 200h50v100h-40v-50h-10z"}]},
                {"id":57,"renderings":[{"type":"circle","cx":1,"cy":2,"r":3}]},
                {"id":58,"renderings":[{"type":"polygon","points":"1,2 3,4"}]},
                {"id":300,"renderings":[{"type":"rect","rect":{"x":0,"y":0,"width":1,"height":1}}]}
            ]}]}]}"#;
        let metadata: Metadata = serde_json::from_str(json).expect("depot parses");
        let regions = metadata.key_regions();
        assert_eq!(
            regions,
            vec![
                super::KeyRegion {
                    usage: 41,
                    rect: super::Rect {
                        x: 10.,
                        y: 20.,
                        width: 30.,
                        height: 40.
                    },
                    outline: false,
                },
                super::KeyRegion {
                    usage: 40,
                    rect: super::Rect {
                        x: 100.,
                        y: 200.,
                        width: 50.,
                        height: 100.
                    },
                    outline: true,
                },
            ]
        );
    }

    /// A zone list in a shape the parser does not expect costs the zones,
    /// not the hotspot data around them.
    #[test]
    fn malformed_zones_keep_the_rest_of_the_file() {
        let json = r#"{"images":[{"key":"device_image","origin":{"width":10,"height":20},
            "zones":[{"id":"PERKEY_KEYBOARD","components":[{"id":"Esc"}]}]}]}"#;
        let metadata: Metadata = serde_json::from_str(json).expect("depot parses");
        assert_eq!(metadata.images[0].origin.height, 20);
        assert!(metadata.key_regions().is_empty());
    }

    #[test]
    fn depot_without_zones_has_no_key_regions() {
        let json = r#"{"images":[{"key":"device_image","origin":{"width":10,"height":10}}]}"#;
        let metadata: Metadata = serde_json::from_str(json).expect("depot parses");
        assert!(metadata.key_regions().is_empty());
    }

    #[test]
    fn path_bounds_follows_relative_curves_and_implicit_line_tos() {
        let rect = super::path_bounds("M0,0 10,0 c0,5 5,10 10,10 L-2-3.5e0").expect("bounds");
        assert_eq!(
            rect,
            super::Rect {
                x: -2.,
                y: -3.5,
                width: 22.,
                height: 13.5
            }
        );
        assert!(super::path_bounds("M0 0 a5 5 0 0 1 10 0").is_none());
        assert!(super::path_bounds("").is_none());
    }
}

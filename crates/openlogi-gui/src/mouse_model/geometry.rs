//! Geometry helpers for the centre mouse model.
//!
//! These functions keep Logitech asset coordinate translation and fallback
//! label layout separate from the GPUI element tree in `view`.

use crate::asset::ResolvedAsset;
use crate::data::mouse_buttons::{ButtonId, Hotspot, MOUSE_MODEL_SIZE};
use crate::mouse_model::leader_lines::{Label, Side};

/// Approx pixel width of each hotspot hit-target. Logitech only gives us a
/// marker point per button, not a rectangle, so we size by hand.
const ASSET_HOTSPOT: f32 = 56.;

/// Scale the device image to fit a target height while preserving the
/// **actual PNG's** aspect ratio. The metadata's `origin` reports the
/// silhouette bbox inside the PNG, which is typically narrower than the
/// full image (Logi pads transparent strips on both sides); sizing by
/// origin causes `ObjectFit::Contain` to letterbox vertically and pulls
/// every hotspot off the rendered button.
#[allow(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
pub fn asset_dimensions_for_png(asset: &ResolvedAsset, target_h: f32) -> (f32, f32) {
    if asset.png_height == 0 {
        return MOUSE_MODEL_SIZE;
    }
    let w = target_h * (asset.png_width as f32) / (asset.png_height as f32);
    (w, target_h)
}

/// Convert Logitech's percent-based markers into mouse-local pixel rects,
/// translating from the metadata's "origin" coord system (the silhouette
/// bbox) into the actual rendered PNG coord system.
///
/// Logi's markers are percentages of `origin` (the silhouette bbox).
/// Within the actual PNG, that bbox is centred with equal padding on the
/// left and right. We render at the *PNG's* full aspect (no letterboxing)
/// so the marker translation is:
///
/// ```text
/// bbox_w_rendered = mouse_w * origin.width  / png.width
/// bbox_x_offset   = (mouse_w - bbox_w_rendered) / 2
/// hotspot.x       = bbox_x_offset + marker.x / 100 * bbox_w_rendered
/// hotspot.y       = marker.y / 100 * mouse_h     // height ratio is 1:1
/// ```
///
/// Primary left/right clicks deliberately have no entry — Logi never
/// exposes them as remappable (and Options+ doesn't either), so we don't
/// invent markers for them.
#[allow(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
pub fn asset_hotspots_for_png(asset: &ResolvedAsset, mouse_w: f32, mouse_h: f32) -> Vec<Hotspot> {
    let png_w = asset.png_width as f32;
    let origin_w = asset
        .metadata
        .origin()
        .map_or(png_w, |o| o.width as f32)
        .min(png_w);
    let bbox_w_rendered = if png_w > 0. {
        mouse_w * origin_w / png_w
    } else {
        mouse_w
    };
    let bbox_x_offset = (mouse_w - bbox_w_rendered) / 2.;
    let marker_to_canvas = |mx: f32, my: f32| -> (f32, f32) {
        let cx = bbox_x_offset + mx / 100. * bbox_w_rendered;
        let cy = my / 100. * mouse_h;
        (cx, cy)
    };

    asset
        .metadata
        .assignments()
        .filter_map(|a| {
            let id = map_slot_name(&a.slot_name)?;
            let (cx, cy) = marker_to_canvas(a.marker.x, a.marker.y);
            Some(Hotspot {
                id,
                x: cx - ASSET_HOTSPOT / 2.,
                y: cy - ASSET_HOTSPOT / 2.,
                w: ASSET_HOTSPOT,
                h: ASSET_HOTSPOT,
            })
        })
        .collect()
}

/// Lay labels out on the left side, evenly spaced down the mouse's vertical
/// extent. Slots are assigned in order of the hotspots' y position (top
/// hotspot → top label) so leader lines don't cross.
#[allow(
    clippy::cast_precision_loss,
    reason = "hotspot count is bounded by ButtonId variants — well under f32 mantissa"
)]
pub fn labels_from_hotspots(hotspots: &[Hotspot]) -> Vec<Label> {
    if hotspots.is_empty() {
        return Vec::new();
    }
    let mouse_h = MOUSE_MODEL_SIZE.1;
    let step = mouse_h / (hotspots.len() as f32 + 1.);

    let mut ranks: Vec<usize> = (0..hotspots.len()).collect();
    ranks.sort_by(|&a, &b| {
        hotspots[a]
            .center()
            .1
            .partial_cmp(&hotspots[b].center().1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut slot_of: Vec<usize> = vec![0; hotspots.len()];
    for (rank, idx) in ranks.into_iter().enumerate() {
        slot_of[idx] = rank;
    }

    hotspots
        .iter()
        .enumerate()
        .map(|(i, h)| Label {
            id: h.id,
            side: Side::Left,
            y: step * (slot_of[i] as f32 + 1.),
        })
        .collect()
}

/// Label positions for the synthetic fallback silhouette.
pub fn default_labels() -> Vec<Label> {
    vec![
        Label {
            id: ButtonId::MiddleClick,
            side: Side::Left,
            y: 120.,
        },
        Label {
            id: ButtonId::Back,
            side: Side::Left,
            y: 240.,
        },
        Label {
            id: ButtonId::Forward,
            side: Side::Left,
            y: 340.,
        },
        Label {
            id: ButtonId::DpiToggle,
            side: Side::Left,
            y: 430.,
        },
        Label {
            id: ButtonId::GestureButton,
            side: Side::Left,
            y: 510.,
        },
    ]
}

/// Logitech's stable slot vocabulary → OpenLogi's `ButtonId`. Intentionally
/// conservative; unknown names fall through so widening `ButtonId` later
/// doesn't break old depots.
fn map_slot_name(name: &str) -> Option<ButtonId> {
    match name {
        "SLOT_NAME_LEFT_BUTTON" => Some(ButtonId::LeftClick),
        "SLOT_NAME_RIGHT_BUTTON" => Some(ButtonId::RightClick),
        "SLOT_NAME_MIDDLE_BUTTON" => Some(ButtonId::MiddleClick),
        "SLOT_NAME_BACK_BUTTON" => Some(ButtonId::Back),
        "SLOT_NAME_FORWARD_BUTTON" => Some(ButtonId::Forward),
        "SLOT_NAME_MODESHIFT_BUTTON" => Some(ButtonId::DpiToggle),
        "SLOT_NAME_THUMBWHEEL" => Some(ButtonId::Thumbwheel),
        "SLOT_NAME_GESTURE_BUTTON" => Some(ButtonId::GestureButton),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::mouse_buttons::default_hotspots;

    #[test]
    fn default_labels_include_the_gesture_button() {
        let labels = default_labels();
        assert!(
            labels
                .iter()
                .any(|l| matches!(l.id, ButtonId::GestureButton)),
            "the gesture button needs a fallback label"
        );
    }

    #[test]
    fn labels_track_hotspots_and_avoid_crossing() {
        let hotspots = default_hotspots();
        let labels = labels_from_hotspots(&hotspots);
        assert_eq!(labels.len(), hotspots.len());

        let mut ys: Vec<f32> = labels.iter().map(|l| l.y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ys.dedup();
        assert_eq!(ys.len(), labels.len(), "each label gets a distinct slot");
    }
}

//! Geometry helpers for the centre mouse model.
//!
//! These functions keep Logitech asset coordinate translation and fallback
//! label layout separate from the GPUI element tree in `view`.

use openlogi_core::binding::ButtonId;

use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId};
use super::leader_lines::{Label, Side};
use crate::services::assets::ResolvedAsset;

/// Approx pixel width of each hotspot hit-target. Logitech only gives us a
/// marker point per button, not a rectangle, so we size by hand.
const ASSET_HOTSPOT: f32 = 56.;

/// Scale the device image to *fit inside* a `max_w` × `target_h` box while
/// preserving the **actual PNG's** aspect ratio. A tall device (a mouse) is
/// bound by the height; a wide one (a keyboard) is bound by the width — which
/// is what stops a wide keyboard render from overflowing the panel (#272).
///
/// The metadata's `origin` reports the silhouette bbox inside the PNG, which
/// is typically narrower than the full image (Logi pads transparent strips on
/// both sides); sizing by origin causes `ObjectFit::Contain` to letterbox
/// vertically and pulls every hotspot off the rendered button.
#[expect(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
pub fn asset_dimensions_for_png(asset: &ResolvedAsset, target_h: f32, max_w: f32) -> (f32, f32) {
    if asset.png_height == 0 {
        return MOUSE_MODEL_SIZE;
    }
    let aspect = (asset.png_width as f32) / (asset.png_height as f32);
    let w = target_h * aspect;
    if w > max_w {
        (max_w, max_w / aspect)
    } else {
        (w, target_h)
    }
}

/// Whether the asset exposes any remappable button markers. Mice do (so the
/// model reserves a side gutter for their leader-line labels); keyboards and
/// other label-less devices don't, so the model can hand them the full width.
pub fn asset_has_button_labels(asset: &ResolvedAsset) -> bool {
    asset
        .metadata
        .assignments()
        .any(|a| map_slot_name(&a.slot_name).is_some())
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
#[expect(
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

    let hotspots: Vec<Hotspot> = asset
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
        .collect();

    hotspots
}

/// Lay labels out on the left side, evenly spaced down the mouse's vertical
/// extent. Slots are assigned in order of the hotspots' y position (top
/// hotspot → top label) so leader lines don't cross.
#[expect(
    clippy::cast_precision_loss,
    reason = "hotspot count is bounded by ButtonId variants — well under f32 mantissa"
)]
pub fn labels_from_hotspots(hotspots: &[Hotspot], mouse_h: f32) -> Vec<Label> {
    if hotspots.is_empty() {
        return Vec::new();
    }
    // Even vertical slots across the (possibly scaled) model height, so the
    // labels track the model when it shrinks to fit the viewport.
    let step = mouse_h / (hotspots.len() as f32 + 1.);

    let mut ranks: Vec<usize> = (0..hotspots.len()).collect();
    ranks.sort_by(|&a, &b| hotspots[a].center().1.total_cmp(&hotspots[b].center().1));
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
pub fn default_labels(thumbwheel: bool) -> Vec<Label> {
    let layout: &[(MouseControlId, f32)] = if thumbwheel {
        &[
            (MouseControlId::Button(ButtonId::MiddleClick), 80.),
            (MouseControlId::ThumbwheelRotation, 165.),
            (MouseControlId::Button(ButtonId::Back), 250.),
            (MouseControlId::Button(ButtonId::Forward), 335.),
            (MouseControlId::Button(ButtonId::DpiToggle), 420.),
            (MouseControlId::Button(ButtonId::GestureButton), 505.),
        ]
    } else {
        &[
            (MouseControlId::Button(ButtonId::MiddleClick), 120.),
            (MouseControlId::Button(ButtonId::Back), 240.),
            (MouseControlId::Button(ButtonId::Forward), 340.),
            (MouseControlId::Button(ButtonId::DpiToggle), 430.),
            (MouseControlId::Button(ButtonId::GestureButton), 510.),
        ]
    };
    layout
        .iter()
        .map(|(id, y)| Label {
            id: *id,
            side: Side::Left,
            y: *y,
        })
        .collect()
}

/// Logitech's stable slot vocabulary → OpenLogi's visual control IDs. Intentionally
/// conservative; unknown names fall through so widening `MouseControlId` later
/// doesn't break old depots.
fn map_slot_name(name: &str) -> Option<MouseControlId> {
    match name {
        "SLOT_NAME_LEFT_BUTTON" => Some(MouseControlId::Button(ButtonId::LeftClick)),
        "SLOT_NAME_RIGHT_BUTTON" => Some(MouseControlId::Button(ButtonId::RightClick)),
        "SLOT_NAME_MIDDLE_BUTTON" => Some(MouseControlId::Button(ButtonId::MiddleClick)),
        // The main wheel's tilt. Logi names the two slots after the scroll they
        // produce in firmware; each is its own reprogrammable control
        // (`0x1b04` CIDs `0x005b` / `0x005d`), not part of the middle click.
        "SLOT_NAME_LEFT_SCROLL_BUTTON" => Some(MouseControlId::Button(ButtonId::WheelTiltLeft)),
        "SLOT_NAME_RIGHT_SCROLL_BUTTON" => Some(MouseControlId::Button(ButtonId::WheelTiltRight)),
        "SLOT_NAME_BACK_BUTTON" => Some(MouseControlId::Button(ButtonId::Back)),
        "SLOT_NAME_FORWARD_BUTTON" => Some(MouseControlId::Button(ButtonId::Forward)),
        "SLOT_NAME_MODESHIFT_BUTTON" => Some(MouseControlId::Button(ButtonId::DpiToggle)),
        "SLOT_NAME_THUMBWHEEL" => Some(MouseControlId::ThumbwheelRotation),
        "SLOT_NAME_GESTURE_BUTTON" => Some(MouseControlId::Button(ButtonId::GestureButton)),
        // The MX Master 4 Haptic Sense Panel. Logi names the slot after its
        // Options+ default assignment (the radial Actions Ring menu), but the
        // marker is the panel itself.
        "ASSIGNMENT_NAME_SHOW_RADIAL_MENU" => Some(MouseControlId::Button(ButtonId::HapticPanel)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::mouse::hotspots::default_hotspots;

    #[test]
    fn default_labels_include_capability_gated_thumbwheel() {
        assert!(
            !default_labels(false)
                .iter()
                .any(|label| label.id == MouseControlId::ThumbwheelRotation)
        );
        assert_eq!(
            default_labels(true)
                .iter()
                .filter(|label| label.id == MouseControlId::ThumbwheelRotation)
                .count(),
            1
        );
    }

    #[test]
    fn thumbwheel_metadata_maps_to_one_rotation_control() {
        assert_eq!(
            map_slot_name("SLOT_NAME_THUMBWHEEL"),
            Some(MouseControlId::ThumbwheelRotation)
        );
    }

    #[test]
    fn wheel_tilt_slots_map_to_their_own_controls() {
        // Logi ships the tilt as two slots named after the scroll it produces
        // (MX Anywhere 2S and friends). Leaving them unmapped is what used to
        // hide the tilt from the buttons panel entirely.
        assert_eq!(
            map_slot_name("SLOT_NAME_LEFT_SCROLL_BUTTON"),
            Some(MouseControlId::Button(ButtonId::WheelTiltLeft))
        );
        assert_eq!(
            map_slot_name("SLOT_NAME_RIGHT_SCROLL_BUTTON"),
            Some(MouseControlId::Button(ButtonId::WheelTiltRight))
        );
    }

    #[test]
    fn labels_track_hotspots_and_avoid_crossing() {
        let hotspots = default_hotspots(true);
        let labels = labels_from_hotspots(&hotspots, MOUSE_MODEL_SIZE.1);
        assert_eq!(labels.len(), hotspots.len());

        let mut ys: Vec<f32> = labels.iter().map(|l| l.y).collect();
        ys.sort_by(f32::total_cmp);
        ys.dedup();
        assert_eq!(ys.len(), labels.len(), "each label gets a distinct slot");
    }
}

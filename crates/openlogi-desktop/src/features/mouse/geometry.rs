//! Geometry helpers for the centre mouse model.
//!
//! These functions keep Logitech asset coordinate translation and fallback
//! label layout separate from the GPUI element tree in `view`.

use openlogi_assets::ImageEntry;
use openlogi_core::binding::{ButtonId, GamingLayout};

use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId};
use super::leader_lines::{Label, Side};
use crate::services::assets::{ResolvedAsset, SIDE_VIEW_KEY};

const FRONT_VIEW_KEY: &str = "device_image";

/// Approx pixel width of each hotspot hit-target. Logitech only gives us a
/// marker point per button, not a rectangle, so we size by hand.
const ASSET_HOTSPOT: f32 = 56.;

/// Height of a side-label card. The layout needs it to group related cards
/// without allowing them to overlap at the minimum model height.
pub(super) const LABEL_H: f32 = 56.;

/// Empty space between the grouped Back and Forward cards when the viewport
/// has enough room to pull them closer than the regular even spacing.
const NAVIGATION_GROUP_GAP: f32 = 16.;

/// Whether label cards occupy one or both sides of the device render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelDistribution {
    LeftOnly,
    BothSides,
}

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
    let side_w = asset.side_view.as_ref().map_or(0., |side| {
        side.png_width as f32 * asset.png_height as f32 / side.png_height.max(1) as f32
    });
    let aspect = (asset.png_width as f32 + side_w) / (asset.png_height as f32);
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
pub fn asset_has_button_labels(asset: &ResolvedAsset, layout: Option<&GamingLayout>) -> bool {
    asset
        .metadata
        .assignments()
        .any(|a| map_slot_name(&a.slot_name).is_some())
        || layout.is_some_and(|layout| {
            [FRONT_VIEW_KEY, SIDE_VIEW_KEY]
                .into_iter()
                .filter_map(|key| asset.metadata.image(key))
                .any(|image| !gaming_controls(image, layout).is_empty())
        })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
pub fn side_view_width(asset: &ResolvedAsset, mouse_h: f32) -> f32 {
    asset.side_view.as_ref().map_or(0., |side| {
        side.png_width as f32 * mouse_h / side.png_height.max(1) as f32
    })
}

fn gaming_controls(image: &ImageEntry, layout: &GamingLayout) -> Vec<(ButtonId, f32, f32)> {
    let mut controls: Vec<(ButtonId, f32, f32)> = Vec::new();
    for assignment in &image.assignments {
        let Some(button) = assignment
            .g_number()
            .and_then(|number| layout.button_for_g_number(number))
            .filter(|button| layout.remappable().any(|b| b == *button))
        else {
            continue;
        };
        if !controls.iter().any(|(b, ..)| *b == button) {
            controls.push((button, assignment.marker.x, assignment.marker.y));
        }
    }
    controls
}

#[expect(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
fn gaming_hotspots(
    image: &ImageEntry,
    layout: &GamingLayout,
    left: f32,
    w: f32,
    h: f32,
) -> impl Iterator<Item = Hotspot> {
    let (origin_w, origin_h) = (
        image.origin.width.max(1) as f32,
        image.origin.height.max(1) as f32,
    );
    gaming_controls(image, layout)
        .into_iter()
        .map(move |(button, mx, my)| {
            let cx = left + mx / origin_w * w;
            let cy = my / origin_h * h;
            Hotspot {
                id: button.into(),
                x: cx - ASSET_HOTSPOT / 2.,
                y: cy - ASSET_HOTSPOT / 2.,
                w: ASSET_HOTSPOT,
                h: ASSET_HOTSPOT,
            }
        })
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
///
/// G-series depots also mark a side render, drawn left of the front one.
pub fn asset_hotspots_for_png(
    asset: &ResolvedAsset,
    layout: Option<&GamingLayout>,
    mouse_w: f32,
    mouse_h: f32,
) -> Vec<Hotspot> {
    let side_w = side_view_width(asset, mouse_h);
    let mut hotspots = main_view_hotspots(asset, mouse_w - side_w, mouse_h)
        .into_iter()
        .map(|hotspot| Hotspot {
            x: hotspot.x + side_w,
            ..hotspot
        })
        .collect::<Vec<_>>();
    if let Some(layout) = layout {
        if asset.side_view.is_some()
            && let Some(image) = asset.metadata.image(SIDE_VIEW_KEY)
        {
            hotspots.extend(gaming_hotspots(image, layout, 0., side_w, mouse_h));
        }
        if let Some(image) = asset.metadata.image(FRONT_VIEW_KEY) {
            let new = gaming_hotspots(image, layout, side_w, mouse_w - side_w, mouse_h)
                .filter(|hotspot| !hotspots.iter().any(|h| h.id == hotspot.id))
                .collect::<Vec<_>>();
            hotspots.extend(new);
        }
    }
    hotspots
}

#[expect(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
fn main_view_hotspots(asset: &ResolvedAsset, mouse_w: f32, mouse_h: f32) -> Vec<Hotspot> {
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

/// Lay labels out evenly down one or both sides of the mouse. A two-sided
/// layout sends the leftmost half of the hotspots left and the rightmost half
/// right, then orders each side by hotspot height. Back and Forward stay
/// adjacent when both are on the same side because they form one navigation
/// pair, even when another marker sits between them.
#[expect(
    clippy::cast_precision_loss,
    reason = "hotspot count is bounded by ButtonId variants — well under f32 mantissa"
)]
pub fn labels_from_hotspots(
    hotspots: &[Hotspot],
    mouse_h: f32,
    distribution: LabelDistribution,
) -> Vec<Label> {
    if hotspots.is_empty() {
        return Vec::new();
    }

    let mut labels: Vec<Label> = hotspots
        .iter()
        .map(|hotspot| Label {
            id: hotspot.id,
            side: Side::Left,
            y: 0.,
        })
        .collect();
    if distribution == LabelDistribution::BothSides {
        let mut horizontal_order: Vec<usize> = (0..hotspots.len()).collect();
        horizontal_order
            .sort_by(|&a, &b| hotspots[a].center().0.total_cmp(&hotspots[b].center().0));
        for index in horizontal_order
            .into_iter()
            .skip(hotspots.len().div_ceil(2))
        {
            labels[index].side = Side::Right;
        }
    }

    for side in [Side::Left, Side::Right] {
        let mut vertical_order: Vec<usize> = labels
            .iter()
            .enumerate()
            .filter_map(|(index, label)| (label.side == side).then_some(index))
            .collect();
        vertical_order.sort_by(|&a, &b| hotspots[a].center().1.total_cmp(&hotspots[b].center().1));
        let back = vertical_order
            .iter()
            .position(|&index| labels[index].id == ButtonId::Back.into());
        let forward = vertical_order
            .iter()
            .position(|&index| labels[index].id == ButtonId::Forward.into());
        let navigation_pair = if let (Some(back), Some(forward)) = (back, forward) {
            let first = back.min(forward);
            let second = back.max(forward);
            if second > first + 1 {
                let navigation_button = vertical_order.remove(second);
                vertical_order.insert(first + 1, navigation_button);
            }
            Some((vertical_order[first], vertical_order[first + 1]))
        } else {
            None
        };
        let step = mouse_h / (vertical_order.len() as f32 + 1.);
        for (slot, index) in vertical_order.into_iter().enumerate() {
            labels[index].y = step * (slot as f32 + 1.);
        }
        if let Some((first, second)) = navigation_pair {
            let grouped_step = step.min(LABEL_H + NAVIGATION_GROUP_GAP);
            let adjustment = (step - grouped_step) / 2.;
            labels[first].y += adjustment;
            labels[second].y -= adjustment;
        }
    }

    labels
}

/// Label positions for the synthetic fallback silhouette.
pub fn default_labels(thumbwheel: bool, distribution: LabelDistribution) -> Vec<Label> {
    labels_from_hotspots(
        &super::hotspots::default_hotspots(thumbwheel),
        MOUSE_MODEL_SIZE.1,
        distribution,
    )
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
        "SLOT_NAME_LEFT_SCROLL_BUTTON" | "SLOT_NAME_SCROLL_LEFT" => {
            Some(MouseControlId::Button(ButtonId::WheelTiltLeft))
        }
        "SLOT_NAME_RIGHT_SCROLL_BUTTON" | "SLOT_NAME_SCROLL_RIGHT" => {
            Some(MouseControlId::Button(ButtonId::WheelTiltRight))
        }
        "SLOT_NAME_BACK_BUTTON" => Some(MouseControlId::Button(ButtonId::Back)),
        "SLOT_NAME_FORWARD_BUTTON" => Some(MouseControlId::Button(ButtonId::Forward)),
        "SLOT_NAME_MODESHIFT_BUTTON" | "SLOT_NAME_DPI_BUTTON" => {
            Some(MouseControlId::Button(ButtonId::DpiToggle))
        }
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

    fn g502_asset() -> ResolvedAsset {
        let metadata: openlogi_assets::Metadata = serde_json::from_str(
            r#"{"images":[
              {"key":"device_image","origin":{"width":1391,"height":2700},"assignments":[
                {"slotId":"g502wireless_g1_m1","marker":{"x":538,"y":614}},
                {"slotId":"g502wireless_g3_m1","marker":{"x":800,"y":869}},
                {"slotId":"g502wireless_g7_m1","marker":{"x":295,"y":989}},
                {"slotId":"g502wireless_g10_m1","marker":{"x":900,"y":869}}]},
              {"key":"device_side","origin":{"width":936,"height":2700},"assignments":[
                {"slotId":"g502wireless_g4_m1","marker":{"x":580,"y":1800}}]}]}"#,
        )
        .expect("metadata parses");
        ResolvedAsset {
            depot: "g502_wireless".into(),
            display_name: "G502 Lightspeed".into(),
            kind: None,
            image_path: "front.png".into(),
            hero_image_path: None,
            glow: None,
            metadata,
            png_width: 1391,
            png_height: 2700,
            side_view: Some(crate::services::assets::SideView {
                image_path: "side.png".into(),
                png_width: 936,
                png_height: 2700,
            }),
        }
    }

    #[test]
    fn g502_markers_land_on_their_view_and_skip_the_primary_clicks() {
        let asset = g502_asset();
        let layout = GamingLayout::for_model_key("0407f");
        let (w, h) = asset_dimensions_for_png(&asset, 540., 1000.);
        assert!((w - 465.4).abs() < 0.1 && (h - 540.).abs() < f32::EPSILON);
        let hotspots = asset_hotspots_for_png(&asset, layout, w, h);
        let center = |button: ButtonId| {
            hotspots
                .iter()
                .find(|h| h.id == button.into())
                .map(Hotspot::center)
                .expect("hotspot present")
        };
        let (back_x, back_y) = center(ButtonId::Back);
        assert!((back_x - 116.).abs() < 0.1 && (back_y - 360.).abs() < 0.1);
        let (g7_x, _) = center(ButtonId::G7);
        assert!(
            (g7_x - (187.2 + 59.)).abs() < 0.1,
            "front markers sit right of the side view"
        );
        center(ButtonId::MiddleClick);
        center(ButtonId::WheelTiltRight);
        assert!(!hotspots.iter().any(|h| h.id == ButtonId::LeftClick.into()));
        assert!(asset_has_button_labels(&asset, layout));
        assert!(!asset_has_button_labels(&asset, None));
        assert!(asset_hotspots_for_png(&asset, None, w, h).is_empty());
    }

    #[test]
    fn side_markers_need_the_side_image() {
        let asset = ResolvedAsset {
            side_view: None,
            ..g502_asset()
        };
        let layout = GamingLayout::for_model_key("0407f");
        let (w, h) = asset_dimensions_for_png(&asset, 540., 1000.);
        let hotspots = asset_hotspots_for_png(&asset, layout, w, h);
        assert!(!hotspots.iter().any(|h| h.id == ButtonId::Back.into()));
        assert!(hotspots.iter().any(|h| h.id == ButtonId::G7.into()));
    }

    #[test]
    fn default_labels_include_capability_gated_thumbwheel() {
        assert!(
            !default_labels(false, LabelDistribution::LeftOnly)
                .iter()
                .any(|label| label.id == MouseControlId::ThumbwheelRotation)
        );
        assert_eq!(
            default_labels(true, LabelDistribution::LeftOnly)
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
    fn dpi_slot_names_map_to_dpi_toggle_button() {
        assert_eq!(
            map_slot_name("SLOT_NAME_MODESHIFT_BUTTON"),
            Some(MouseControlId::Button(ButtonId::DpiToggle))
        );
        assert_eq!(
            map_slot_name("SLOT_NAME_DPI_BUTTON"),
            Some(MouseControlId::Button(ButtonId::DpiToggle))
        );
    }

    #[test]
    fn wheel_tilt_slot_names_map_to_their_own_controls() {
        // MX Anywhere uses the longer names; MX Ergo uses the shorter aliases.
        for name in ["SLOT_NAME_LEFT_SCROLL_BUTTON", "SLOT_NAME_SCROLL_LEFT"] {
            assert_eq!(
                map_slot_name(name),
                Some(MouseControlId::Button(ButtonId::WheelTiltLeft))
            );
        }
        for name in ["SLOT_NAME_RIGHT_SCROLL_BUTTON", "SLOT_NAME_SCROLL_RIGHT"] {
            assert_eq!(
                map_slot_name(name),
                Some(MouseControlId::Button(ButtonId::WheelTiltRight))
            );
        }
    }

    #[test]
    fn labels_track_hotspots_and_avoid_crossing() {
        let hotspots = default_hotspots(true);
        let labels =
            labels_from_hotspots(&hotspots, MOUSE_MODEL_SIZE.1, LabelDistribution::LeftOnly);
        assert_eq!(labels.len(), hotspots.len());

        let mut ys: Vec<f32> = labels.iter().map(|l| l.y).collect();
        ys.sort_by(f32::total_cmp);
        ys.dedup();
        assert_eq!(ys.len(), labels.len(), "each label gets a distinct slot");
    }

    #[test]
    fn navigation_labels_stay_together_when_haptic_marker_sits_between() {
        let hotspots = [
            Hotspot {
                id: ButtonId::Forward.into(),
                x: 0.,
                y: 100.,
                w: 10.,
                h: 10.,
            },
            Hotspot {
                id: ButtonId::HapticPanel.into(),
                x: 0.,
                y: 200.,
                w: 10.,
                h: 10.,
            },
            Hotspot {
                id: ButtonId::Back.into(),
                x: 0.,
                y: 300.,
                w: 10.,
                h: 10.,
            },
        ];

        let mut labels =
            labels_from_hotspots(&hotspots, MOUSE_MODEL_SIZE.1, LabelDistribution::LeftOnly);
        labels.sort_by(|a, b| a.y.total_cmp(&b.y));

        assert_eq!(
            labels.iter().map(|label| label.id).collect::<Vec<_>>(),
            [
                MouseControlId::Button(ButtonId::Forward),
                MouseControlId::Button(ButtonId::Back),
                MouseControlId::Button(ButtonId::HapticPanel),
            ]
        );
        let navigation_gap = labels[1].y - labels[0].y;
        let haptic_gap = labels[2].y - labels[1].y;
        assert!(navigation_gap < haptic_gap);
        assert!(navigation_gap >= LABEL_H);
    }

    #[test]
    fn a_two_sided_layout_uses_both_sides() {
        let hotspots = default_hotspots(true);
        let labels =
            labels_from_hotspots(&hotspots, MOUSE_MODEL_SIZE.1, LabelDistribution::BothSides);

        assert!(labels.iter().any(|label| label.side == Side::Left));
        assert!(labels.iter().any(|label| label.side == Side::Right));
    }
}

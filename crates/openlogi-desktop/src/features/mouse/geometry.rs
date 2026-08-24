//! Geometry helpers for the centre mouse model.
//!
//! These functions keep Logitech asset coordinate translation and fallback
//! label layout separate from the GPUI element tree in `view`.

use openlogi_assets::metadata::Assignment;
use openlogi_core::binding::ButtonId;

use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId};
use super::leader_lines::{Label, Side};
use crate::services::assets::ResolvedAsset;

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
        .any(|a| map_assignment(a).is_some())
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
    // The origin of the image the markers were calibrated against — not the
    // depot's first entry. A gaming depot's views differ in width (the G502's
    // `device_image` is 1391, its `device_side` 936), so taking the wrong one
    // scales every hotspot off its control.
    let origin = asset.metadata.buttons_origin();
    let origin_w = origin.map_or(png_w, |o| o.width as f32).min(png_w);
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

    // Options+ depots express markers as a percentage of the origin box;
    // gaming depots express them as absolute pixels within it (a G502 marker
    // reads x=1110 against a 1391-wide origin). The two are told apart by the
    // same signal that picks the slot vocabulary — a gaming depot has no
    // `slotName` — so no magnitude heuristic is involved and the percentage
    // path is untouched for every depot that already worked.
    let to_percent = |value: f32, extent: Option<u32>| -> f32 {
        match extent {
            Some(extent) if extent > 0 => value / extent as f32 * 100.,
            _ => value,
        }
    };

    let hotspots: Vec<Hotspot> = asset
        .metadata
        .assignments()
        .filter_map(|a| {
            let id = map_assignment(a)?;
            let (mx, my) = if a.slot_name.is_empty() {
                (
                    to_percent(a.marker.x, origin.map(|o| o.width)),
                    to_percent(a.marker.y, origin.map(|o| o.height)),
                )
            } else {
                (a.marker.x, a.marker.y)
            };
            let (cx, cy) = marker_to_canvas(mx, my);
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

/// Gaming depots give no `slotName` — only a `slotId` like
/// `g502wireless_g4_m1`, whose `_g<N>_` index is Logitech's G-button number.
///
/// G1–G5 are the five standard HID mouse buttons in order (left, right,
/// middle, back, forward); Logitech's G-numbering follows that ordering
/// across their gaming mice, and the G502's own renders confirm it — the
/// side view has "G5" printed on the forward thumb button and "G4" on the
/// rear one.
///
/// G6 and up are model-specific — sniper, DPI paddles, profile cycle, wheel
/// tilt, in an order that differs per mouse — so they are deliberately left
/// unmapped rather than guessed.
///
/// Nothing is lost by that today: those controls are handled in firmware and
/// emit no HID button event, so the OS hook never sees them, and rebinding
/// them would mean writing the mouse's onboard button map over `0x8100`
/// OnboardProfiles — which OpenLogi does not implement. Mapping them here
/// would only draw hotspots that cannot take a binding.
fn map_slot_id(slot_id: &str) -> Option<MouseControlId> {
    let index: u8 = slot_id
        .rsplit_once("_m")?
        .0
        .rsplit_once("_g")?
        .1
        .parse()
        .ok()?;
    let button = match index {
        1 => ButtonId::LeftClick,
        2 => ButtonId::RightClick,
        3 => ButtonId::MiddleClick,
        4 => ButtonId::Back,
        5 => ButtonId::Forward,
        _ => return None,
    };
    Some(MouseControlId::Button(button))
}

/// The visual control an assignment targets, whichever vocabulary its depot
/// speaks: `slotName` when present, else the `slotId`'s G-number.
fn map_assignment(assignment: &Assignment) -> Option<MouseControlId> {
    if assignment.slot_name.is_empty() {
        map_slot_id(&assignment.slot_id)
    } else {
        map_slot_name(&assignment.slot_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::mouse::hotspots::default_hotspots;

    /// Verbatim from the shipped `g502_wireless` depot: no `device_buttons_image`
    /// entry, no `slotName` on any assignment, markers in absolute origin
    /// pixels, and two views of different widths. Every one of those differs
    /// from the Options+ depots the mapper was written against, so the fixture
    /// is copied from the real file rather than shaped to pass.
    fn g502_metadata() -> openlogi_assets::metadata::Metadata {
        use openlogi_assets::metadata::{
            Assignment, Direction, ImageEntry, Metadata, Origin, Point,
        };
        let assignment = |slot_id: &str, x: f32, y: f32| Assignment {
            slot_name: String::new(),
            slot_id: slot_id.to_string(),
            marker: Point { x, y },
            label: Direction { x: -1400, y: 0 },
        };
        Metadata {
            images: vec![
                ImageEntry {
                    key: "device_image".to_string(),
                    origin: Origin {
                        width: 1391,
                        height: 2700,
                    },
                    assignments: vec![
                        assignment("g502wireless_g1_m1", 538., 614.),
                        assignment("g502wireless_g3_m1", 800., 869.),
                        assignment("g502wireless_g9_m1", 815., 1411.),
                    ],
                },
                ImageEntry {
                    key: "device_side".to_string(),
                    origin: Origin {
                        width: 936,
                        height: 2700,
                    },
                    assignments: vec![
                        assignment("g502wireless_g4_m1", 580., 1800.),
                        assignment("g502wireless_g5_m1", 500., 1400.),
                        assignment("g502wireless_g6_m1", 270., 1250.),
                    ],
                },
            ],
        }
    }

    /// G1–G5 are the standard HID buttons; G6 and up are model-specific and
    /// must stay unmapped rather than be guessed at.
    #[test]
    fn gaming_slot_ids_map_only_the_standard_hid_buttons() {
        assert_eq!(
            map_slot_id("g502wireless_g3_m1"),
            Some(MouseControlId::Button(ButtonId::MiddleClick))
        );
        assert_eq!(
            map_slot_id("g502wireless_g4_m1"),
            Some(MouseControlId::Button(ButtonId::Back))
        );
        assert_eq!(
            map_slot_id("g502wireless_g5_m1"),
            Some(MouseControlId::Button(ButtonId::Forward))
        );
        for unmapped in ["g502wireless_g6_m1", "g502wireless_g11_m1"] {
            assert_eq!(
                map_slot_id(unmapped),
                None,
                "{unmapped} is model-specific and must not be guessed"
            );
        }
        assert_eq!(map_slot_id("mx-master-6b012_c83"), None);
    }

    /// The buttons panel must read the depot's side view, not its first
    /// image entry — that is where a gaming mouse's thumb buttons live, and
    /// its origin is a different width than the hero render's.
    #[test]
    fn gaming_depot_assignments_come_from_the_side_view() {
        let meta = g502_metadata();
        let origin = meta.buttons_origin().expect("side view carries an origin");
        assert_eq!((origin.width, origin.height), (936, 2700));
        let slots: Vec<&str> = meta.assignments().map(|a| a.slot_id.as_str()).collect();
        assert_eq!(
            slots,
            vec![
                "g502wireless_g4_m1",
                "g502wireless_g5_m1",
                "g502wireless_g6_m1"
            ]
        );
    }

    /// Markers in a gaming depot are absolute pixels in the origin box, not
    /// the 0..100 percentage Options+ depots use. Feeding them through the
    /// percentage path unchanged puts every hotspot far off the canvas, so
    /// this pins that they land on the render.
    #[test]
    fn gaming_markers_are_normalized_onto_the_canvas() {
        let asset = ResolvedAsset {
            depot: "g502_wireless".to_string(),
            display_name: "G502".to_string(),
            kind: openlogi_core::device::DeviceKind::Mouse,
            image_path: std::path::PathBuf::from("/tmp/side.png"),
            hero_image_path: None,
            glow: None,
            metadata: g502_metadata(),
            png_width: 936,
            png_height: 2700,
        };
        let (mouse_w, mouse_h) = (300., 866.);
        let hotspots = asset_hotspots_for_png(&asset, mouse_w, mouse_h);

        assert_eq!(
            hotspots.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![
                MouseControlId::Button(ButtonId::Back),
                MouseControlId::Button(ButtonId::Forward),
            ],
            "G6 has no mapping, so only the two thumb buttons survive"
        );
        for hotspot in &hotspots {
            let (cx, cy) = hotspot.center();
            assert!(
                (0. ..=mouse_w).contains(&cx) && (0. ..=mouse_h).contains(&cy),
                "hotspot {:?} landed at ({cx}, {cy}), outside the {mouse_w}×{mouse_h} render",
                hotspot.id
            );
        }
        // G4 is the rear thumb button and G5 the forward one, so G4 must sit
        // lower on a render whose nose points up.
        assert!(hotspots[0].center().1 > hotspots[1].center().1);
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

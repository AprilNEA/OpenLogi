//! Geometry helpers for the centre mouse model.
//!
//! These functions keep Logitech asset coordinate translation and fallback
//! label layout separate from the GPUI element tree in `view`.

use openlogi_assets::ImageEntry;
use openlogi_core::binding::{ButtonId, GamingLayout};

use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId};
use super::leader_lines::{self, Label, Side};
use crate::services::assets::{ResolvedAsset, SIDE_VIEW_KEY};
use crate::ui::callouts::{self, spread_in_order};

const FRONT_VIEW_KEY: &str = "device_image";

/// Approx pixel width of each hotspot hit-target. Logitech only gives us a
/// marker point per button, not a rectangle, so we size by hand.
const ASSET_HOTSPOT: f32 = 56.;

/// How far, as a fraction of the PNG's height, the metadata `origin.height`
/// may drift from the real PNG before [`asset_hotspots_for_png`] treats the
/// whole `origin` block as a stale placeholder and stops cropping by it.
const ORIGIN_HEIGHT_TOLERANCE: f32 = 0.05;

/// Height of a side-label card whose value fits on one line. The layout
/// needs it to group related cards without allowing them to overlap at the
/// minimum model height.
pub(super) const LABEL_H: f32 = 56.;

/// Empty space kept between two neighbouring label cards on one side.
const LABEL_GAP: f32 = 12.;

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
/// The `hotspot.y` line only holds if `origin.height` actually equals the
/// PNG's height — some depots (seen on the M720 Triathlon: `origin` is a
/// stale `396×396` placeholder reused across every image entry, against a
/// real `1228×1920` PNG) violate that, and trusting `origin.width` there
/// crushes every hotspot into a narrow sliver instead of spreading them
/// across the render. When `origin.height` is not within
/// [`ORIGIN_HEIGHT_TOLERANCE`] of the PNG's height, the whole `origin` block
/// is untrustworthy, so we fall back to treating it as the full PNG (no
/// crop) rather than compressing hotspots with a bogus width.
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
    let png_h = asset.png_height as f32;
    let origin_w = asset
        .metadata
        .origin()
        .filter(|o| {
            png_h > 0. && (o.height as f32 - png_h).abs() <= png_h * ORIGIN_HEIGHT_TOLERANCE
        })
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

/// Lay the labels down one or both sides of the mouse, each level with its
/// control where there is room. A two-sided layout sends the leftmost half of
/// the hotspots left and the rightmost half right, then orders each side by
/// hotspot height. Cards that would overlap are pushed apart as little as
/// possible without changing that order (see [`spread_in_order`]).
///
/// Back and Forward stay adjacent when both are on the same side because they
/// form one navigation pair, even when another marker sits between them. That
/// grouping is kept only while every leader on the side can still reach its
/// card without crossing another; otherwise the side falls back to plain
/// height order, which never crosses.
///
/// `room_h` is the height the cards may use, centred on the model and at
/// least `mouse_h`: a model shrunk to fit a narrow window leaves height
/// beside it that a crowded side needs (see [`place_side`]). Every card is
/// `card_h` tall.
pub fn labels_from_hotspots(
    hotspots: &[Hotspot],
    mouse_w: f32,
    mouse_h: f32,
    room_h: f32,
    distribution: LabelDistribution,
    card_h: f32,
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
            .skip(left_side_count(hotspots.len(), distribution))
        {
            labels[index].side = Side::Right;
        }
    }

    for side in [Side::Left, Side::Right] {
        let on_side: Vec<usize> = labels
            .iter()
            .enumerate()
            .filter_map(|(index, label)| (label.side == side).then_some(index))
            .collect();
        let by_height = height_order(hotspots, &on_side);
        let grouped = with_navigation_pair(by_height.clone(), &labels);
        place_side(&mut labels, hotspots, &grouped, mouse_h, room_h, card_h);
        if side_leaders_cross(hotspots, &labels, &grouped, mouse_w) {
            place_side(&mut labels, hotspots, &by_height, mouse_h, room_h, card_h);
        }
    }

    labels
}

/// How many of `count` labels go on the left: all of them, or the leftmost
/// half (rounded up) when both sides hold labels. The left is the busier side.
fn left_side_count(count: usize, distribution: LabelDistribution) -> usize {
    match distribution {
        LabelDistribution::LeftOnly => count,
        LabelDistribution::BothSides => count.div_ceil(2),
    }
}

/// Whether `count` labels of `card_h` fit `room_h` with every gap between
/// them kept, on their busier side.
#[expect(
    clippy::cast_precision_loss,
    reason = "a side holds a handful of cards — well within f32 mantissa"
)]
pub fn labels_fit(count: usize, distribution: LabelDistribution, card_h: f32, room_h: f32) -> bool {
    let side = left_side_count(count, distribution) as f32;
    side * card_h + (side - 1.).max(0.) * LABEL_GAP <= room_h
}

/// One side's labels top to bottom by hotspot height.
fn height_order(hotspots: &[Hotspot], on_side: &[usize]) -> Vec<usize> {
    let mut order = on_side.to_vec();
    order.sort_by(|&a, &b| hotspots[a].center().1.total_cmp(&hotspots[b].center().1));
    order
}

/// `order` with Back and Forward pulled together: the lower of the two moves
/// up to sit right after the higher when other cards sit between them.
fn with_navigation_pair(mut order: Vec<usize>, labels: &[Label]) -> Vec<usize> {
    let back = order
        .iter()
        .position(|&index| labels[index].id == ButtonId::Back.into());
    let forward = order
        .iter()
        .position(|&index| labels[index].id == ButtonId::Forward.into());
    if let (Some(back), Some(forward)) = (back, forward) {
        let first = back.min(forward);
        let second = back.max(forward);
        if second > first + 1 {
            let navigation_button = order.remove(second);
            order.insert(first + 1, navigation_button);
        }
    }
    order
}

/// Put `order`'s cards as close to their hotspots' heights as the card
/// height `card_h` and the gap between cards allow. A side that does not fit
/// the model's height reaches past its top and bottom into `room_h`, only as
/// far as it needs; one that does not fit `room_h` either closes its gaps, so
/// every card stays on screen.
#[expect(
    clippy::cast_precision_loss,
    reason = "a side holds a handful of cards — well within f32 mantissa"
)]
fn place_side(
    labels: &mut [Label],
    hotspots: &[Hotspot],
    order: &[usize],
    mouse_h: f32,
    room_h: f32,
    card_h: f32,
) {
    let ideal: Vec<f32> = order.iter().map(|&i| hotspots[i].center().1).collect();
    let pitch = card_h + LABEL_GAP;
    let span = order.len().saturating_sub(1) as f32 * pitch;
    let overhang = ((span - (mouse_h - card_h)) / 2.).clamp(0., ((room_h - mouse_h) / 2.).max(0.));
    let lo = card_h / 2. - overhang;
    let hi = mouse_h - card_h / 2. + overhang;
    let spacing = match order.len() {
        0 | 1 => pitch,
        count => pitch.min((hi - lo) / (count - 1) as f32),
    };
    let ys = spread_in_order(&ideal, spacing, lo, hi);
    for (&index, y) in order.iter().zip(ys) {
        labels[index].y = y;
    }
}

/// Whether any two leaders of `order` cross. Only the part over the model
/// and the first [`leader_lines::STUB`] of the gutter can cross; past that
/// every leader runs straight at its own card's height.
fn side_leaders_cross(
    hotspots: &[Hotspot],
    labels: &[Label],
    order: &[usize],
    mouse_w: f32,
) -> bool {
    let routes: Vec<Vec<(f32, f32)>> = order
        .iter()
        .map(|&i| {
            leader_lines::route(
                hotspots[i].center(),
                labels[i].side,
                labels[i].y,
                mouse_w,
                leader_lines::STUB,
            )
        })
        .collect();
    routes.iter().enumerate().any(|(a, route)| {
        routes[a + 1..]
            .iter()
            .any(|other| callouts::routes_cross(route, other))
    })
}

/// The fallback silhouette's hotspots for a device without an image. A
/// G-series mouse keeps only the buttons its layout has: the generic DPI and
/// gesture buttons are `0x1b04` controls it cannot capture. The layout's
/// other buttons get placed hotspots, so each one can still be opened.
pub fn fallback_hotspots(layout: Option<&GamingLayout>, thumbwheel: bool) -> Vec<Hotspot> {
    let mut hotspots = super::hotspots::default_hotspots(thumbwheel);
    hotspots.retain(|hotspot| on_fallback(layout, hotspot));
    for button in layout.into_iter().flat_map(GamingLayout::remappable) {
        if !hotspots
            .iter()
            .any(|hotspot| hotspot.id.button() == Some(button))
            && let Some(hotspot) = super::hotspots::gaming_fallback_hotspot(button)
        {
            hotspots.push(hotspot);
        }
    }
    hotspots
}

/// Whether the fallback silhouette shows `hotspot` for a device with `layout`.
fn on_fallback(layout: Option<&GamingLayout>, hotspot: &Hotspot) -> bool {
    layout.is_none_or(|layout| {
        hotspot
            .id
            .button()
            .is_some_and(|button| layout.remappable().any(|b| b == button))
    })
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
        // MX Ergo (CID 0x00ED) and MX Ergo S (CID 0x00FD) name their
        // precision-mode/DPI-shift button after its Options+ function.
        "SLOT_NAME_MODESHIFT_BUTTON"
        | "SLOT_NAME_DPI_BUTTON"
        | "SLOT_NAME_PRECISION_MODE_BUTTON"
        | "SLOT_NAME_CHANGE_POINTER_SPEED" => Some(MouseControlId::Button(ButtonId::DpiToggle)),
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
    use std::path::PathBuf;

    use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
    use openlogi_core::device::DeviceKind;

    use super::*;
    use crate::features::mouse::hotspots::default_hotspots;

    /// Label positions for the synthetic fallback silhouette at its authored
    /// size.
    fn default_labels(
        layout: Option<&GamingLayout>,
        thumbwheel: bool,
        distribution: LabelDistribution,
    ) -> Vec<Label> {
        labels_from_hotspots(
            &fallback_hotspots(layout, thumbwheel),
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            distribution,
            LABEL_H,
        )
    }

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

    /// A gaming mouse without an image keeps only what it can deliver: the
    /// DPI toggle, the gesture button and the thumb wheel are HID++-only
    /// controls, and offering them would take a binding and drop it. A model
    /// nobody has dumped offers what the OS hook remaps; a dumped one also
    /// gets placed hotspots for every other button its layout remaps.
    #[test]
    fn an_undivertable_mouse_without_an_image_keeps_only_what_it_delivers() {
        use openlogi_core::binding::STANDARD_BUTTONS;
        let ids = |layout| -> Vec<_> {
            fallback_hotspots(layout, true)
                .iter()
                .map(|hotspot| hotspot.id)
                .collect()
        };
        let os_hook: Vec<MouseControlId> =
            [ButtonId::MiddleClick, ButtonId::Back, ButtonId::Forward]
                .map(Into::into)
                .into();
        assert_eq!(ids(Some(&STANDARD_BUTTONS)), os_hook);

        let g502 = GamingLayout::for_model_key("0407f").expect("the G502 layout");
        let mut every: Vec<MouseControlId> = g502.remappable().map(Into::into).collect();
        let mut drawn = ids(Some(g502));
        every.sort_by_key(|id| format!("{id:?}"));
        drawn.sort_by_key(|id| format!("{id:?}"));
        assert_eq!(drawn, every, "every remappable button can be opened");

        assert_eq!(fallback_hotspots(None, true), default_hotspots(true));
    }

    #[test]
    fn an_unverified_gaming_depot_marks_only_the_standard_buttons() {
        use openlogi_core::binding::STANDARD_BUTTONS;
        let asset = g502_asset();
        let (w, h) = asset_dimensions_for_png(&asset, 540., 1000.);
        let mut ids: Vec<_> = asset_hotspots_for_png(&asset, Some(&STANDARD_BUTTONS), w, h)
            .iter()
            .map(|hotspot| hotspot.id)
            .collect();
        ids.sort_by_key(|id| format!("{id:?}"));
        assert_eq!(
            ids,
            [ButtonId::Back.into(), ButtonId::MiddleClick.into()],
            "the fixture marks G1, G3, G7, G10 and G4"
        );
    }

    #[test]
    fn default_labels_include_capability_gated_thumbwheel() {
        assert!(
            !default_labels(None, false, LabelDistribution::LeftOnly)
                .iter()
                .any(|label| label.id == MouseControlId::ThumbwheelRotation)
        );
        assert_eq!(
            default_labels(None, true, LabelDistribution::LeftOnly)
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
        // MX Ergo S and MX Ergo name their precision-mode button differently.
        assert_eq!(
            map_slot_name("SLOT_NAME_CHANGE_POINTER_SPEED"),
            Some(MouseControlId::Button(ButtonId::DpiToggle))
        );
        assert_eq!(
            map_slot_name("SLOT_NAME_PRECISION_MODE_BUTTON"),
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

    fn hotspot(id: impl Into<MouseControlId>, x: f32, y: f32) -> Hotspot {
        Hotspot {
            id: id.into(),
            x,
            y,
            w: 10.,
            h: 10.,
        }
    }

    fn label_for(labels: &[Label], id: impl Into<MouseControlId>) -> Label {
        let id = id.into();
        *labels.iter().find(|label| label.id == id).expect("label")
    }

    /// Every side keeps its cards apart and its leaders uncrossed.
    fn assert_clean_layout(hotspots: &[Hotspot], labels: &[Label], mouse_w: f32) {
        for side in [Side::Left, Side::Right] {
            let mut ys: Vec<f32> = labels
                .iter()
                .filter(|label| label.side == side)
                .map(|label| label.y)
                .collect();
            ys.sort_by(f32::total_cmp);
            for pair in ys.windows(2) {
                assert!(
                    pair[1] - pair[0] >= LABEL_H + LABEL_GAP - 0.01,
                    "cards overlap on {side:?}: {ys:?}"
                );
            }
            let routes: Vec<_> = hotspots
                .iter()
                .zip(labels)
                .filter(|(_, label)| label.side == side)
                .map(|(hotspot, label)| {
                    leader_lines::route(hotspot.center(), side, label.y, mouse_w, 24.)
                })
                .collect();
            for (a, route) in routes.iter().enumerate() {
                for other in &routes[a + 1..] {
                    assert!(
                        !callouts::routes_cross(route, other),
                        "leaders cross on {side:?}: {route:?} / {other:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn labels_sit_level_with_their_controls_when_there_is_room() {
        let hotspots = [
            hotspot(ButtonId::MiddleClick, 200., 95.),
            hotspot(ButtonId::DpiToggle, 220., 295.),
        ];
        let labels = labels_from_hotspots(
            &hotspots,
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            LabelDistribution::LeftOnly,
            LABEL_H,
        );
        assert_eq!(label_for(&labels, ButtonId::MiddleClick).y, 100.);
        assert_eq!(label_for(&labels, ButtonId::DpiToggle).y, 300.);
    }

    /// The room beside the model is only for a side that does not fit it.
    #[test]
    fn a_side_that_fits_the_model_ignores_the_room_beside_it() {
        let hotspots = [
            hotspot(ButtonId::MiddleClick, 200., 5.),
            hotspot(ButtonId::Forward, 200., 195.),
            hotspot(ButtonId::Back, 220., 215.),
        ];
        let at = |room_h| -> Vec<f32> {
            labels_from_hotspots(
                &hotspots,
                MOUSE_MODEL_SIZE.0,
                MOUSE_MODEL_SIZE.1,
                room_h,
                LabelDistribution::LeftOnly,
                LABEL_H,
            )
            .iter()
            .map(|label| label.y)
            .collect()
        };
        assert_eq!(at(MOUSE_MODEL_SIZE.1), at(MOUSE_MODEL_SIZE.1 + 300.));
    }

    #[test]
    fn crowded_labels_spread_evenly_around_their_controls() {
        let hotspots = [
            hotspot(ButtonId::Forward, 200., 195.),
            hotspot(ButtonId::Back, 220., 215.),
        ];
        let labels = labels_from_hotspots(
            &hotspots,
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            LabelDistribution::LeftOnly,
            LABEL_H,
        );
        let forward = label_for(&labels, ButtonId::Forward).y;
        let back = label_for(&labels, ButtonId::Back).y;
        assert!((back - forward - (LABEL_H + LABEL_GAP)).abs() < 0.01);
        assert!((f32::midpoint(forward, back) - 210.).abs() < 0.01);
    }

    #[test]
    fn labels_track_hotspots_and_avoid_crossing() {
        for distribution in [LabelDistribution::LeftOnly, LabelDistribution::BothSides] {
            let hotspots = default_hotspots(true);
            let labels = labels_from_hotspots(
                &hotspots,
                MOUSE_MODEL_SIZE.0,
                MOUSE_MODEL_SIZE.1,
                MOUSE_MODEL_SIZE.1,
                distribution,
                LABEL_H,
            );
            assert_eq!(labels.len(), hotspots.len());
            assert_clean_layout(&hotspots, &labels, MOUSE_MODEL_SIZE.0);
        }
    }

    /// The MX Master 3's gesture button sits under the thumb, level with
    /// Back. Back's leader runs above it, so the navigation pair stays
    /// together without a crossing.
    #[test]
    fn navigation_labels_stay_together_when_a_marker_beside_them_sits_between() {
        let hotspots = [
            hotspot(ButtonId::Forward, 200., 100.),
            hotspot(ButtonId::GestureButton, 40., 280.),
            hotspot(ButtonId::Back, 200., 300.),
        ];
        let mut labels = labels_from_hotspots(
            &hotspots,
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            LabelDistribution::LeftOnly,
            LABEL_H,
        );
        assert_clean_layout(&hotspots, &labels, MOUSE_MODEL_SIZE.0);
        labels.sort_by(|a, b| a.y.total_cmp(&b.y));
        assert_eq!(
            labels.iter().map(|label| label.id).collect::<Vec<_>>(),
            [
                MouseControlId::Button(ButtonId::Forward),
                MouseControlId::Button(ButtonId::Back),
                MouseControlId::Button(ButtonId::GestureButton),
            ]
        );
    }

    /// A marker in line with the pair would force Back's leader across its
    /// own, so the side keeps plain height order instead.
    #[test]
    fn a_marker_in_line_with_the_navigation_pair_keeps_height_order() {
        let hotspots = [
            hotspot(ButtonId::Forward, 0., 100.),
            hotspot(ButtonId::HapticPanel, 0., 200.),
            hotspot(ButtonId::Back, 0., 300.),
        ];
        let mut labels = labels_from_hotspots(
            &hotspots,
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            LabelDistribution::LeftOnly,
            LABEL_H,
        );
        assert_clean_layout(&hotspots, &labels, MOUSE_MODEL_SIZE.0);
        labels.sort_by(|a, b| a.y.total_cmp(&b.y));
        assert_eq!(
            labels.iter().map(|label| label.id).collect::<Vec<_>>(),
            [
                MouseControlId::Button(ButtonId::Forward),
                MouseControlId::Button(ButtonId::HapticPanel),
                MouseControlId::Button(ButtonId::Back),
            ]
        );
    }

    #[test]
    fn a_two_sided_layout_uses_both_sides() {
        let hotspots = default_hotspots(true);
        let labels = labels_from_hotspots(
            &hotspots,
            MOUSE_MODEL_SIZE.0,
            MOUSE_MODEL_SIZE.1,
            MOUSE_MODEL_SIZE.1,
            LabelDistribution::BothSides,
            LABEL_H,
        );

        assert!(labels.iter().any(|label| label.side == Side::Left));
        assert!(labels.iter().any(|label| label.side == Side::Right));
    }

    fn mouse_asset(
        origin: (u32, u32),
        png: (u32, u32),
        markers: &[(&str, f32, f32)],
    ) -> ResolvedAsset {
        let assignments = markers
            .iter()
            .map(|(slot_name, x, y)| Assignment {
                slot_id: String::new(),
                slot_name: (*slot_name).to_string(),
                marker: Point { x: *x, y: *y },
                label: Direction { x: 0, y: 0 },
            })
            .collect();
        ResolvedAsset {
            depot: "m720_triathlon".to_string(),
            display_name: "M720 Triathlon".to_string(),
            kind: Some(DeviceKind::Mouse),
            image_path: PathBuf::from("/tmp/side.png"),
            hero_image_path: None,
            glow: None,
            metadata: Metadata {
                images: vec![
                    ImageEntry {
                        key: "device_image".to_string(),
                        origin: Origin {
                            width: origin.0,
                            height: origin.1,
                        },
                        assignments: Vec::new(),
                    },
                    ImageEntry {
                        key: "device_buttons_image".to_string(),
                        origin: Origin {
                            width: origin.0,
                            height: origin.1,
                        },
                        assignments,
                    },
                ],
            },
            png_width: png.0,
            png_height: png.1,
            side_view: None,
        }
    }

    #[test]
    fn stale_placeholder_origin_falls_back_to_the_full_png_width() {
        // Reproduces the M720 Triathlon depot: every image entry carries a
        // stale 396x396 placeholder `origin` against a real 1228x1920 PNG.
        // Trusting `origin.width` there crushes every hotspot into a
        // ~32%-wide sliver centred on the render instead of spreading them
        // across it.
        let asset = mouse_asset(
            (396, 396),
            (1228, 1920),
            &[("SLOT_NAME_MIDDLE_BUTTON", 66., 14.)],
        );

        let hotspots = asset_hotspots_for_png(&asset, None, 300., 470.);

        assert_eq!(hotspots.len(), 1);
        let (cx, _) = hotspots[0].center();
        assert!((cx - 0.66 * 300.).abs() < 1., "cx={cx}");
    }

    #[test]
    fn a_correct_origin_still_applies_the_horizontal_crop() {
        // A depot whose `origin.height` genuinely matches the PNG (Logi pads
        // transparent strips on the sides only) must still get the crop
        // correction — the fallback must not fire on valid data. The marker
        // sits off-centre: a centred one lands on the same pixel with or
        // without the crop, so it could not catch a broken correction.
        let asset = mouse_asset(
            (900, 1920),
            (1228, 1920),
            &[("SLOT_NAME_MIDDLE_BUTTON", 25., 14.)],
        );

        let hotspots = asset_hotspots_for_png(&asset, None, 300., 470.);

        assert_eq!(hotspots.len(), 1);
        let (cx, _) = hotspots[0].center();
        let bbox_w = 300. * 900. / 1228.;
        let expected = (300. - bbox_w) / 2. + 0.25 * bbox_w;
        assert!((cx - expected).abs() < 1., "cx={cx}");
    }
}

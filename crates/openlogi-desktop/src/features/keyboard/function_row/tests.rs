use super::key_points::{KeyPoint, key_slots};
use super::*;
use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
use openlogi_core::binding::ButtonId;
use openlogi_core::config::FunctionKey;
use openlogi_core::device::DeviceKind;
use std::path::PathBuf;

/// The F-row the fallback shows: Esc then F1–F19.
const FUNCTION_KEYS: [FunctionKey; 20] = FunctionKey::ALL;

fn key_points(asset: Option<&ResolvedAsset>) -> Vec<KeyPoint> {
    key_slots(asset)
        .into_iter()
        .map(|slot| slot.point)
        .collect()
}

#[test]
fn clicking_the_selected_key_closes_the_panel() {
    assert_eq!(next_selection_after_click(None, 3), Some(3));
    assert_eq!(next_selection_after_click(Some(3), 3), None);
    assert_eq!(next_selection_after_click(Some(3), 4), Some(4));
}

#[test]
fn hover_or_selection_highlights_a_key() {
    assert!(key_is_highlighted(2, Some(2), None));
    assert!(key_is_highlighted(2, None, Some(2)));
    assert!(key_is_highlighted(2, Some(2), Some(7)));
    assert!(!key_is_highlighted(2, Some(1), Some(7)));
}

#[test]
fn function_row_covers_esc_through_f19() {
    let labels: Vec<&str> = FUNCTION_KEYS.iter().map(|key| key.label()).collect();

    assert_eq!(FUNCTION_KEYS.len(), 20);
    assert_eq!(labels.first(), Some(&"Esc"));
    assert_eq!(labels.last(), Some(&"F19"));
    assert!(labels.contains(&"F13"));
    assert!(labels.contains(&"F19"));
}

#[test]
fn fallback_key_positions_cover_the_full_top_row() {
    let positions = key_x_fractions(None);

    assert_eq!(positions.len(), 20);
    assert_eq!(positions.first().copied(), Some(EVEN_SPACING_START));
    assert_eq!(positions.last().copied(), Some(EVEN_SPACING_END));
    // Without markers the tab falls back to the OS-hook F-row.
    let slots = key_slots(None);
    assert!(matches!(slots[0].target, KeyTarget::FunctionKey(ref t) if t.to_string() == "esc"));
    assert_eq!(slots[1].legend, "F1");
}

/// A Logi depot with control markers shows the keyboard's own controls —
/// one per marked control, named by the catalog, in physical reading order —
/// and never the OS-hook F-row. This is the MX Keys Mini: Easy-Switch takes
/// F1–F3, so the first marked key is F4's backlight control (#711).
#[test]
fn control_markers_become_control_slots_in_reading_order() {
    rust_i18n::set_locale("en");
    let asset = asset_with_controls(&[
        (0x00e2, 27.9, 13.8),
        (0x00e3, 34.3, 13.8),
        (0x0103, 40.75, 13.8),
        (0x0108, 47.1, 13.8),
        (0x010a, 53.5, 13.8),
        (0x011c, 59.9, 13.8),
        (0x00e5, 66.3, 13.8),
        (0x00e7, 72.7, 13.8),
        (0x00e8, 79.1, 13.8),
        (0x00e9, 85.5, 13.8),
    ]);

    let slots = key_slots(Some(&asset));

    assert_eq!(slots.len(), 10, "every marked control, no phantom F-keys");
    let targets: Vec<_> = slots.iter().map(|slot| slot.target.clone()).collect();
    assert_eq!(
        targets[0],
        KeyTarget::Control(ButtonId::control(0x00e2)),
        "the first marked key is the backlight control, not F1"
    );
    assert_eq!(targets[4], KeyTarget::Control(ButtonId::control(0x010a)));
    assert_eq!(slots[4].legend, "Screen Capture Key");
    assert_approx_eq(slots[4].point.x_frac, 0.535 + 0.02);
    assert_approx_eq(slots[4].point.y_frac, 0.138 + 0.023);
    assert!(
        slots
            .windows(2)
            .all(|pair| pair[0].point.x_frac < pair[1].point.x_frac),
        "one row stays in physical left-to-right order"
    );
}

/// Keys down the right edge (Home/End/PgUp/PgDn on an MX Mechanical Mini)
/// follow the top row, and a control OpenLogi has no catalog row for is
/// still a key, named by its number.
#[test]
fn control_slots_read_row_by_row_and_name_unknown_controls_by_number() {
    rust_i18n::set_locale("en");
    let asset = asset_with_controls(&[
        (0x0118, 93.6, 25.3),
        (0x0119, 93.6, 39.7),
        (0x01f3, 93.6, 54.0),
        (0x011c, 56.7, 11.0),
        (0x00d4, 63.0, 11.0),
    ]);

    let slots = key_slots(Some(&asset));

    let legends: Vec<&str> = slots.iter().map(|slot| slot.legend.as_str()).collect();
    assert_eq!(
        legends,
        [
            "Mic Mute Key",
            "Search Key",
            "Home Key",
            "End Key",
            "Control 0x01f3",
        ]
    );
}

/// The same control marked twice (a depot that repeats a key under
/// `device_buttons_image`) is one slot; Easy-Switch host keys are not keys.
#[test]
fn control_slots_dedupe_repeats_and_skip_easy_switch() {
    let mut asset = asset_with_controls(&[(0x010a, 53.5, 13.8), (0x010a, 53.5, 13.8)]);
    asset.metadata.images.push(ImageEntry {
        key: "device_easyswitch_image".to_string(),
        origin: Origin {
            width: 1872,
            height: 728,
        },
        assignments: vec![control_assignment(0x00d1, 8.65, 13.8)],
    });

    let slots = key_slots(Some(&asset));

    assert_eq!(slots.len(), 1);
    assert_eq!(
        slots[0].target,
        KeyTarget::Control(ButtonId::control(0x010a))
    );
}

/// The G513 family's `metadata_full.json`: `device_image` markers in
/// absolute pixels of the authored canvas, which matches the cached
/// render. F1-F12 come from the markers; Esc is synthesized one chassis
/// offset left of F1.
#[test]
fn g513_pixel_markers_resolve_esc_plus_f1_to_f12() {
    let marker_xs = [
        285., 405., 525., 645., 840., 960., 1080., 1200., 1395., 1515., 1635., 1755.,
    ];
    let asset = legacy_asset(&marker_xs, 290., (2760, 1600), (2760, 1600));

    let points = key_points(Some(&asset));

    assert_eq!(points.len(), 13, "Esc + F1-F12, no phantom F13-F19");
    assert_approx_eq(points[1].x_frac, 285. / 2760.);
    assert_approx_eq(points[12].x_frac, 1755. / 2760.);
    // Esc: 1.55 key pitches (median gap 120px) left of F1.
    assert_approx_eq(points[0].x_frac, (285. - 1.55 * 120.) / 2760.);
    for point in &points {
        assert_approx_eq(point.y_frac, 290. / 1600.);
    }
    assert!(
        points
            .windows(2)
            .all(|pair| pair[0].x_frac < pair[1].x_frac),
        "points stay in physical left-to-right order"
    );
}

/// The same depot's `metadata.json` is authored against a *different*
/// render (the G512 banner). Its origin doesn't match the cached PNG, so
/// the markers must be rejected in favour of the even-spacing fallback
/// rather than misplacing every callout.
#[test]
fn pixel_markers_for_a_different_render_fall_back_to_even_spacing() {
    let marker_xs = [370., 525., 680., 835., 1090., 1250., 1400., 1555.];
    let asset = legacy_asset(&marker_xs, 300., (3598, 1315), (2760, 1600));

    let points = key_points(Some(&asset));

    assert_eq!(points.len(), FUNCTION_KEYS.len());
    assert_approx_eq(points[0].x_frac, EVEN_SPACING_START);
    assert_approx_eq(points[19].x_frac, EVEN_SPACING_END);
}

#[test]
fn render_size_follows_the_png_aspect_up_to_the_width_cap() {
    // MX Keys-class render (1872x728): width-bound at a roomy viewport.
    let mx = legacy_asset(&[], 0., (1872, 728), (1872, 728));
    let (w, h) = keyboard_render_size(Some(&mx), 900.);
    assert_approx_eq(w, 700.);
    assert!((h - 700. * 728. / 1872.).abs() < 0.01);

    // G513 render (2760x1600) is far taller at the same width.
    let g513 = legacy_asset(&[], 0., (2760, 1600), (2760, 1600));
    let (w, h) = keyboard_render_size(Some(&g513), 900.);
    assert_approx_eq(w, 700.);
    assert!((h - 700. * 1600. / 2760.).abs() < 0.01);

    // A short viewport shrinks the render instead of overflowing it.
    let (w, h) = keyboard_render_size(Some(&g513), 500.);
    assert_approx_eq(h, KEYBOARD_MIN_IMG_H);
    assert!((w - KEYBOARD_MIN_IMG_H * 2760. / 1600.).abs() < 0.01);

    assert_eq!(keyboard_render_size(None, 900.), FALLBACK_KEYBOARD_SIZE);
}

#[test]
fn callouts_spread_evenly_from_margin_to_margin() {
    let margin = KEY_CALLOUT_W / 2.0 + 4.0;
    assert_approx_eq(callout_center_x(0, 13, 700.0), margin);
    assert_approx_eq(callout_center_x(12, 13, 700.0), 700.0 - margin);
    assert_approx_eq(callout_center_x(0, 1, 700.0), 350.0);
    assert!(callout_left_px(0, 13, 700.0, KEY_CALLOUT_W) >= 0.0);
    assert!(callout_left_px(12, 13, 700.0, KEY_CALLOUT_W) <= 700.0 - KEY_CALLOUT_W);
}

/// Bubbles share a stagger lane with every second key; same-lane
/// neighbours must never overlap for any board size the row can show.
#[test]
fn same_lane_callouts_never_overlap() {
    for count in [13usize, 20] {
        for idx in 0..count.saturating_sub(2) {
            let gap = callout_center_x(idx + 2, count, KEYBOARD_W)
                - callout_center_x(idx, count, KEYBOARD_W);
            assert!(
                gap >= KEY_CALLOUT_W,
                "lane neighbours {idx}/{} overlap at count {count}: gap {gap}",
                idx + 2
            );
        }
    }
}

#[test]
fn function_key_callouts_stagger_even_lower_odd_upper() {
    assert!(callout_top_px(0) > callout_top_px(1));
    assert_eq!(callout_top_px(0), callout_top_px(2));
    assert_eq!(callout_top_px(1), callout_top_px(3));
}

#[test]
#[expect(
    clippy::cast_precision_loss,
    reason = "lane counts are bounded by FUNCTION_KEYS"
)]
fn staggered_function_key_callout_rows_fit_the_keyboard_width() {
    let lower_count = FUNCTION_KEYS
        .iter()
        .enumerate()
        .filter(|(idx, _)| callout_lane_is_lower(*idx))
        .count();
    let upper_count = FUNCTION_KEYS.len() - lower_count;
    assert!(
        KEY_CALLOUT_W * lower_count as f32 <= KEYBOARD_W,
        "lower callout lane overlaps before spacing is considered"
    );
    assert!(
        KEY_CALLOUT_W * upper_count as f32 <= KEYBOARD_W,
        "upper callout lane overlaps before spacing is considered"
    );
}

/// A legacy pixel-marker asset: `device_image` assignments in absolute
/// pixels of an `origin` canvas, over a render of `png` dimensions.
fn legacy_asset(
    marker_xs: &[f32],
    marker_y: f32,
    origin: (u32, u32),
    png: (u32, u32),
) -> ResolvedAsset {
    let assignments = marker_xs
        .iter()
        .map(|x| Assignment {
            slot_id: String::new(),
            slot_name: String::new(),
            marker: Point { x: *x, y: marker_y },
            label: Direction { x: -1, y: -1 },
        })
        .collect();
    ResolvedAsset {
        depot: "g513".to_string(),
        display_name: "G513".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/g513.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata {
            images: vec![ImageEntry {
                key: "device_image".to_string(),
                origin: Origin {
                    width: origin.0,
                    height: origin.1,
                },
                assignments,
            }],
        },
        png_width: png.0,
        png_height: png.1,
    }
}

fn control_assignment(cid: u16, x: f32, y: f32) -> Assignment {
    Assignment {
        slot_id: format!("test-2b369_c{cid}"),
        slot_name: String::new(),
        marker: Point { x, y },
        label: Direction { x: -1, y: -1 },
    }
}

/// An MX Keys-class asset whose `device_keys_image` marks `controls` as
/// `(cid, marker x %, marker y %)`.
fn asset_with_controls(controls: &[(u16, f32, f32)]) -> ResolvedAsset {
    ResolvedAsset {
        depot: "mx_keys_mini".to_string(),
        display_name: "MX Keys Mini".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/mx-keys-mini.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata {
            images: vec![ImageEntry {
                key: "device_keys_image".to_string(),
                origin: Origin {
                    width: 1872,
                    height: 728,
                },
                assignments: controls
                    .iter()
                    .map(|&(cid, x, y)| control_assignment(cid, x, y))
                    .collect(),
            }],
        },
        png_width: 1872,
        png_height: 728,
    }
}

fn assert_approx_eq(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.0001,
        "expected {expected}, got {actual}"
    );
}

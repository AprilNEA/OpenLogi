use super::*;
use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
use openlogi_core::device::{DeviceKind, DeviceModelInfo, DeviceTransports};
use std::path::PathBuf;

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
    let labels: Vec<&str> = FUNCTION_KEYS.iter().map(|(label, _)| *label).collect();

    assert_eq!(FUNCTION_KEYS.len(), 20);
    assert_eq!(labels.first(), Some(&"Esc"));
    assert_eq!(labels.last(), Some(&"F19"));
    assert!(labels.contains(&"F13"));
    assert!(labels.contains(&"F19"));
}

#[test]
fn mx_mechanical_mini_uses_semantic_hidpp_controls_and_physical_markers() {
    let model = DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports {
            btle: true,
            ..DeviceTransports::default()
        },
        model_ids: [MX_MECHANICAL_MINI_MAC_MODEL_ID, 0, 0],
        extended_model_id: 0,
    };
    let asset = mx_mechanical_mini_asset();

    let definitions =
        mx_mechanical_mini_definitions(&model, Some(&asset)).expect("recognized Mini layout");

    assert_eq!(definitions.len(), 20);
    assert_eq!(definitions[1].target, global_target(0x7a));
    assert_approx_eq(definitions[1].point.x_frac, 0.096);
    assert_eq!(
        definitions[13].target,
        BindingTarget::Device(ButtonId::KeyVolumeDown)
    );
    assert_eq!(
        definitions[14].target,
        BindingTarget::Device(ButtonId::KeyVolumeUp)
    );
    assert_eq!(
        definitions[15].target,
        BindingTarget::Device(ButtonId::KeyDoNotDisturb)
    );
    assert_eq!(
        definitions[16].target,
        BindingTarget::Device(ButtonId::KeyHome)
    );
    assert_eq!(
        definitions[17].target,
        BindingTarget::Device(ButtonId::KeyEnd)
    );
    assert_eq!(
        definitions[18].target,
        BindingTarget::Device(ButtonId::KeyPageUp)
    );
    assert_eq!(
        definitions[19].target,
        BindingTarget::Device(ButtonId::KeyPageDown)
    );
    assert_approx_eq(definitions[16].point.y_frac, 0.276);
    assert_approx_eq(definitions[19].point.y_frac, 0.710);
    assert_eq!(definitions[13].label.as_ref(), "Vol Dn");
    assert_eq!(definitions[14].label.as_ref(), "Vol Up");
    assert_eq!(definitions[15].label.as_ref(), "DND");
    assert_eq!(definitions[16].label.as_ref(), "Home");
    assert_eq!(definitions[17].label.as_ref(), "End");
    assert_eq!(definitions[18].label.as_ref(), "Pg Up");
    assert_eq!(definitions[19].label.as_ref(), "Pg Dn");
}

#[test]
fn unrelated_keyboard_does_not_use_the_mini_layout() {
    let model = DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [0x1234, 0, 0],
        extended_model_id: 0,
    };

    assert!(mx_mechanical_mini_definitions(&model, Some(&mx_mechanical_mini_asset())).is_none());
}

#[test]
fn fallback_key_positions_cover_the_full_top_row() {
    let positions = key_x_fractions(None);

    assert_eq!(positions.len(), 20);
    assert_eq!(positions.first().copied(), Some(EVEN_SPACING_START));
    assert_eq!(positions.last().copied(), Some(EVEN_SPACING_END));
}

#[test]
fn mx_keys_markers_merge_function_and_easy_switch_groups() {
    let key_markers = vec![
        9.0, 13.4, 17.8, 22.3, 26.7, 31.15, 35.55, 40.05, 44.55, 49.1, 53.5, 57.9, 62.35, 81.5,
        85.9, 90.3, 94.7,
    ];
    let easy_switch_markers = vec![67.5, 71.92, 76.3];
    let asset = asset_with_markers(&key_markers, &easy_switch_markers);

    let positions = key_x_fractions(Some(&asset));

    assert_eq!(positions.len(), 20);
    assert_approx_eq(positions[0], 0.045);
    assert_approx_eq(positions[1], 0.11);
    assert_approx_eq(positions[12], 0.599);
    assert_approx_eq(positions[13], 0.695);
    assert_approx_eq(positions[15], 0.783);
    assert_approx_eq(positions[16], 0.835);
    assert_approx_eq(positions[19], 0.967);
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "positions should stay in physical left-to-right order"
    );
}

#[test]
fn mx_keys_markers_preserve_key_center_points() {
    let key_markers = vec![
        9.0, 13.4, 17.8, 22.3, 26.7, 31.15, 35.55, 40.05, 44.55, 49.1, 53.5, 57.9, 62.35, 81.5,
        85.9, 90.3, 94.7,
    ];
    let easy_switch_markers = vec![67.5, 71.92, 76.3];
    let asset = asset_with_markers(&key_markers, &easy_switch_markers);

    let points = key_points(Some(&asset));

    assert_eq!(points.len(), 20);
    assert_approx_eq(points[19].x_frac, 0.967);
    assert_approx_eq(points[19].y_frac, 0.153);
    assert_approx_eq(key_target_top_px(points[19].y_frac, 220.0, 30.0), 18.66);
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

fn asset_with_markers(key_markers: &[f32], easy_switch_markers: &[f32]) -> ResolvedAsset {
    ResolvedAsset {
        depot: "mx_keys_s_for_mac".to_string(),
        display_name: "MX Keys S for Mac".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/mx-keys.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata {
            images: vec![
                ImageEntry {
                    key: "device_keys_image".to_string(),
                    origin: Origin {
                        width: 1872,
                        height: 728,
                    },
                    assignments: assignments_from_markers(key_markers),
                },
                ImageEntry {
                    key: "device_easyswitch_image".to_string(),
                    origin: Origin {
                        width: 1872,
                        height: 728,
                    },
                    assignments: assignments_from_markers(easy_switch_markers),
                },
            ],
        },
        png_width: 1872,
        png_height: 728,
    }
}

fn assignments_from_markers(markers: &[f32]) -> Vec<Assignment> {
    markers
        .iter()
        .enumerate()
        .map(|(idx, x)| Assignment {
            slot_name: format!("slot-{idx}"),
            marker: Point { x: *x, y: 13.0 },
            label: Direction { x: -1, y: -1 },
        })
        .collect()
}

fn global_target(keycode: u16) -> BindingTarget {
    BindingTarget::Global(KeyTrigger {
        keycode,
        modifiers: KeyModifiers::default(),
    })
}

fn mx_mechanical_mini_asset() -> ResolvedAsset {
    let top_row = [
        ("SLOT_NAME_BACKLIGHT_DOWN", 25.9, 11.0),
        ("SLOT_NAME_BACKLIGHT_UP", 32.0, 11.0),
        ("SLOT_NAME_DICTATION", 38.15, 11.0),
        ("SLOT_NAME_EMOJI", 44.4, 11.0),
        ("SLOT_NAME_SCREEN_CAPTURE", 50.5, 11.0),
        ("SLOT_NAME_MUTE_UNMUTE_AUDIO", 56.7, 11.0),
        ("SLOT_NAME_SEARCH", 63.0, 11.0),
        ("SLOT_NAME_PLAY_PAUSE", 69.0, 11.0),
        ("SLOT_NAME_MUTE", 75.2, 11.0),
        ("SLOT_NAME_VOLUME_DOWN", 81.3, 11.0),
        ("SLOT_NAME_VOLUME_UP", 87.35, 11.0),
        ("SLOT_NAME_DO_NOT_DISTURB", 93.6, 11.0),
        ("SLOT_NAME_HOME", 93.6, 25.3),
        ("SLOT_NAME_END", 93.6, 39.7),
        ("SLOT_NAME_PAGE_UP", 93.6, 54.0),
        ("SLOT_NAME_PAGE_DOWN", 93.6, 68.7),
    ];
    let easy_switch = [
        ("SLOT_NAME_EASYSWITCH_1", 7.6, 11.0),
        ("SLOT_NAME_EASYSWITCH_2", 13.8, 11.0),
        ("SLOT_NAME_EASYSWITCH_3", 19.8, 11.0),
    ];
    let assignments = |slots: &[(&str, f32, f32)]| {
        slots
            .iter()
            .map(|(slot_name, x, y)| Assignment {
                slot_name: (*slot_name).to_string(),
                marker: Point { x: *x, y: *y },
                label: Direction { x: -1, y: -1 },
            })
            .collect()
    };
    ResolvedAsset {
        depot: "mx_mechanical_mini_mac".to_string(),
        display_name: "MX Mechanical Mini for Mac".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/mx-mechanical-mini.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata {
            images: vec![
                ImageEntry {
                    key: "device_keys_image".to_string(),
                    origin: Origin {
                        width: 1872,
                        height: 728,
                    },
                    assignments: assignments(&top_row),
                },
                ImageEntry {
                    key: "device_easyswitch_image".to_string(),
                    origin: Origin {
                        width: 1872,
                        height: 728,
                    },
                    assignments: assignments(&easy_switch),
                },
            ],
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

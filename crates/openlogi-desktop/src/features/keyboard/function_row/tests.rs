use super::*;
use gpui::{Focusable as _, TestAppContext};
use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
use openlogi_core::config::Config;
use openlogi_core::device::DeviceKind;
use std::path::PathBuf;

use crate::services::assets::AssetResolver;
use crate::state::ConfigPersistence;

fn install_app_state(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            AppState::with_runtime(
                Config::ephemeral(),
                &[],
                &[],
                &cache,
                &[],
                ConfigPersistence::MemoryOnly,
                commands,
            )
        });
        AppState::set_global(state, cx);
    });
}

#[gpui::test]
fn profile_name_inputs_are_created_and_accept_focus(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));

    cx.update(|window, cx| {
        let input = view.update(cx, |view, cx| {
            view.sync_g_profile_name_inputs(
                Some("test-device".to_string()),
                &std::collections::BTreeMap::new(),
                window,
                cx,
            );
            view.profile_name_inputs
                .get(&GKeyProfile::M1)
                .expect("M1 profile-name input should exist")
                .input
                .clone()
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        assert_eq!(view.read(cx).profile_name_inputs.len(), 3);
        assert!(input.read(cx).focus_handle(cx).is_focused(window));
    });
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
fn takeover_and_mode_gate_the_editable_gaming_keys() {
    let available = GamingKeysAvailable {
        g_row: true,
        mode: true,
        macro_record: true,
    };

    assert!(!gaming_selection_ok(
        Some(ButtonId::KeyG1),
        available,
        false,
        GamingKeyMode::Profiles,
    ));
    assert!(gaming_selection_ok(
        Some(ButtonId::KeyG1),
        available,
        true,
        GamingKeyMode::Profiles,
    ));
    assert!(!gaming_selection_ok(
        Some(ButtonId::KeyM2),
        available,
        true,
        GamingKeyMode::Profiles,
    ));
    assert!(gaming_selection_ok(
        Some(ButtonId::KeyM2),
        available,
        true,
        GamingKeyMode::NineButtons,
    ));
    assert!(gaming_selection_ok(
        Some(ButtonId::KeyMr),
        available,
        true,
        GamingKeyMode::NineButtons,
    ));
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

#[test]
fn g913_uses_its_real_esc_and_f1_to_f12_layout() {
    let asset = g913_asset();

    let points = key_points(Some(&asset));
    let labels: Vec<&str> = FUNCTION_KEYS
        .iter()
        .zip(&points)
        .map(|((label, _), _)| *label)
        .collect();

    assert_eq!(points.len(), 13, "G913 has Esc plus F1-F12");
    assert_eq!(labels.first(), Some(&"Esc"));
    assert_eq!(labels.last(), Some(&"F12"));
    assert!(!labels.contains(&"F13"));
    assert_approx_eq(points[0].x_frac, 320. / 3600.);
    assert_approx_eq(points[12].x_frac, 2359. / 3600.);
}

#[test]
fn g913_gaming_diagram_maps_m_above_g_to_physical_keycaps() {
    let asset = g913_asset();
    let gaming = GamingEditorState {
        available: GamingKeysAvailable {
            g_row: true,
            mode: true,
            macro_record: true,
        },
        software_control: true,
        mode: GamingKeyMode::NineButtons,
        profile_bindings: std::collections::BTreeMap::new(),
        profile_names: std::collections::BTreeMap::new(),
        nine_button_bindings: std::collections::BTreeMap::new(),
    };

    let slots = g913_gaming_slots(Some(&asset), &gaming, GKeyProfile::M2);

    assert_eq!(slots.len(), 9);
    assert_eq!(slots[0].button, ButtonId::KeyM1);
    assert_eq!(slots[3].button, ButtonId::KeyMr);
    assert_eq!(slots[4].button, ButtonId::KeyG1);
    assert_eq!(slots[8].button, ButtonId::KeyG5);
    let first_mode_position = gaming_callout_position(&slots[0], gaming.mode);
    let second_mode_position = gaming_callout_position(&slots[1], gaming.mode);
    let first_g_position = gaming_callout_position(&slots[4], gaming.mode);
    let second_g_position = gaming_callout_position(&slots[5], gaming.mode);
    let last_g_position = gaming_callout_position(&slots[8], gaming.mode);
    assert!(
        first_mode_position.1 < first_g_position.1,
        "M keys stay above the G column"
    );
    assert!(
        first_g_position.0 + KEY_CALLOUT_W < G913_G_KEY_GUTTER_W,
        "G keys keep visible distance from the keyboard image"
    );
    assert_approx_eq(
        second_mode_position.0 - first_mode_position.0 - KEY_CALLOUT_W,
        6.,
    );
    let upper_function_center =
        G913_FUNCTION_CALLOUT_OFFSET + KEY_CALLOUT_TOP_UPPER + KEY_CALLOUT_H / 2.;
    let lower_function_center =
        G913_FUNCTION_CALLOUT_OFFSET + KEY_CALLOUT_TOP_LOWER + KEY_CALLOUT_H / 2.;
    assert_approx_eq(
        first_mode_position.1 + KEY_CALLOUT_H / 2.,
        f32::midpoint(upper_function_center, lower_function_center),
    );
    assert_approx_eq(second_mode_position.0, first_g_position.0);
    assert_approx_eq(
        second_g_position.1 - first_g_position.1 - KEY_CALLOUT_H,
        12.,
    );
    let g1_physical_y = G913_CALLOUT_BAND_H + slots[4].y_frac * 218.75;
    assert!(
        first_g_position.1 + KEY_CALLOUT_H < g1_physical_y,
        "G-key leader slopes down-right from the callout to the keycap"
    );
    let keyboard_bottom = G913_CALLOUT_BAND_H + 218.75;
    assert!(
        last_g_position.1 < keyboard_bottom && last_g_position.1 + KEY_CALLOUT_H > keyboard_bottom,
        "G5 straddles the keyboard's lower-left edge"
    );
    assert_approx_eq(slots[0].x_frac, 576. / 3600.);
    assert_approx_eq(slots[4].x_frac, 154. / 3850.);
    assert_approx_eq(slots[8].y_frac, 1099. / 1202.);
}

#[test]
fn g913_mode_keys_do_not_draw_keyboard_leaders() {
    assert!(!gaming_key_has_leader(ButtonId::KeyM1));
    assert!(!gaming_key_has_leader(ButtonId::KeyM2));
    assert!(!gaming_key_has_leader(ButtonId::KeyM3));
    assert!(gaming_key_has_leader(ButtonId::KeyMr));
    assert!(gaming_key_has_leader(ButtonId::KeyG1));
}

#[test]
fn g913_mode_keys_show_saved_profile_names() {
    let gaming = GamingEditorState {
        available: GamingKeysAvailable {
            g_row: true,
            mode: true,
            macro_record: true,
        },
        software_control: true,
        mode: GamingKeyMode::Profiles,
        profile_bindings: std::collections::BTreeMap::new(),
        profile_names: [(GKeyProfile::M2, "Work".to_string())]
            .into_iter()
            .collect(),
        nine_button_bindings: std::collections::BTreeMap::new(),
    };

    let slots = g913_gaming_slots(Some(&g913_asset()), &gaming, GKeyProfile::M2);

    assert_eq!(slots[0].binding.as_ref(), "Click to set");
    assert_eq!(slots[1].binding.as_ref(), "Work");
    assert_eq!(slots[2].binding.as_ref(), "Click to set");
    let m1 = gaming_callout_position(&slots[0], gaming.mode);
    let m2 = gaming_callout_position(&slots[1], gaming.mode);
    let m3 = gaming_callout_position(&slots[2], gaming.mode);
    let g1 = gaming_callout_position(&slots[3], gaming.mode);
    assert_approx_eq(
        gaming_callout_width(ButtonId::KeyM1, gaming.mode),
        PROFILE_M_CALLOUT_W,
    );
    assert_approx_eq(m2.0 - m1.0 - PROFILE_M_CALLOUT_W, GAMING_CALLOUT_GAP);
    assert_approx_eq(m3.0 - m2.0 - PROFILE_M_CALLOUT_W, GAMING_CALLOUT_GAP);
    assert_approx_eq(m2.0, g1.0);
    assert!(m3.0 + PROFILE_M_CALLOUT_W < G913_G_KEY_GUTTER_W);
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

fn g913_asset() -> ResolvedAsset {
    ResolvedAsset {
        depot: "g913".to_string(),
        display_name: "G915".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/g913.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata { images: Vec::new() },
        png_width: 3600,
        png_height: 1125,
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

fn assert_approx_eq(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.0001,
        "expected {expected}, got {actual}"
    );
}

use super::callout_layout::{CalloutLayout, CardRect, callout_layout};
use super::key_points::{KeyPoint, key_slots, photo_key_shape};
use super::*;
use crate::ui::callouts::routes_cross;
use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
use openlogi_core::binding::{ButtonId, Cid, GridKey};
use openlogi_core::config::FunctionKey;
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::{DeviceRoute, KeypadDescriptor};
use std::path::PathBuf;

mod native_function;
/// The MX Creative Keypad, as its wired route names it.
fn mx_keypad() -> Option<&'static KeypadDescriptor> {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc354,
    }
    .keypad()
}

/// The F-row the fallback shows: Esc then F1–F19.
const FUNCTION_KEYS: [FunctionKey; 20] = FunctionKey::ALL;

fn key_points(asset: Option<&ResolvedAsset>) -> Vec<KeyPoint> {
    key_slots(asset, None)
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
fn picking_the_current_action_clears_the_binding() {
    assert_eq!(binding_after_pick(Some(&Action::Copy), Action::Copy), None);
}

#[test]
fn picking_an_action_sets_or_replaces_the_binding() {
    assert_eq!(binding_after_pick(None, Action::Copy), Some(Action::Copy));
    assert_eq!(
        binding_after_pick(Some(&Action::Paste), Action::Copy),
        Some(Action::Copy)
    );
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
    let slots = key_slots(None, None);
    assert!(matches!(slots[0].target, KeyTarget::FunctionKey(ref t) if t.to_string() == "esc"));
    assert_eq!(slots[1].legend, "F1");
}

/// A keypad has no F-row to bind — that row belongs to keyboards, and a
/// chip of it would write a global binding for a key that exists on neither
/// the device nor the plane. What a markerless keypad shows is what it has:
/// its nine grid keys, then the paging controls OpenLogi catalogued.
#[test]
fn a_keypad_without_markers_gets_keycaps_not_the_global_f_row() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let slots = key_slots(None, mx_keypad());

    assert_eq!(slots.len(), 11, "nine grid keys and two paging keys");
    assert!(
        !slots
            .iter()
            .any(|slot| matches!(slot.target, KeyTarget::FunctionKey(_))),
        "no keypad key binds to the global keyboard plane"
    );
    assert_eq!(slots[0].target, KeyTarget::grid(GridKey::TopLeft));
    assert_eq!(slots[0].legend, "Grid Key (Top Left)");
    assert_eq!(slots[4].target, KeyTarget::grid(GridKey::Middle));
    assert_eq!(slots[4].legend, "Grid Key (Middle)");
    assert_eq!(
        slots[9].target,
        KeyTarget::Control(ButtonId::control(0x01a1)),
        "the paging row follows the grid, in left-to-right order"
    );
    assert_eq!(slots[9].legend, "Left Paging Button");
    assert_eq!(
        slots[10].target,
        KeyTarget::Control(ButtonId::control(0x01a2))
    );
}

/// Where the keycaps sit comes from the device model: the grid fills three
/// rows in the same row-major order the firmware reports, and the paging row
/// lies under the first two columns as it does on the chassis.
#[test]
fn keypad_keycaps_follow_the_device_layout() {
    let slots = key_slots(None, mx_keypad());

    for row in 0..3 {
        let y = slots[row * 3].point.y_frac;
        for col in 0..3 {
            let slot = &slots[row * 3 + col];
            assert_approx_eq(slot.point.y_frac, y);
            if col > 0 {
                let left = &slots[row * 3 + col - 1];
                assert!(left.point.x_frac < slot.point.x_frac, "left to right");
            }
        }
        if row > 0 {
            assert!(slots[row * 3].point.y_frac > slots[row * 3 - 3].point.y_frac);
        }
    }
    // Paging: below the grid, lined up under columns one and two.
    assert!(slots[9].point.y_frac > slots[6].point.y_frac);
    assert_approx_eq(slots[9].point.x_frac, slots[6].point.x_frac);
    assert_approx_eq(slots[10].point.x_frac, slots[7].point.x_frac);
}

/// A keypad depot that marks only its paging keys has not answered what the
/// tab asks: the photo path would show two bubbles and hide the nine grid
/// keys it exists to bind. The device-model keycap map — every key, always —
/// stays until the grid is marked too.
#[test]
fn paging_markers_alone_leave_the_keypad_on_the_keycap_map() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let asset = asset_with_controls(&[(0x01a1, 25.20, 82.04), (0x01a2, 49.91, 82.04)]);

    let marked = key_slots(Some(&asset), mx_keypad());

    assert_eq!(marked, key_slots(None, mx_keypad()));
}

/// Logi's own keypad depot marks no key, so the stock render carries the
/// centres OpenLogi measured off it: every key the schematic shows, in the
/// same order and binding the same targets, but on the photo — and on the
/// keys the OpenLogi-authored markers name, two independent measurements of
/// one render.
#[test]
fn the_stock_keypad_render_puts_every_key_on_the_photo() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    let KeySurface::Photo(stock) = key_surface(Some(&stock_keypad_asset((935, 1105))), mx_keypad())
    else {
        panic!("the stock render maps the keys onto the photo");
    };

    let schematic = key_slots(None, mx_keypad());
    assert_eq!(
        stock.iter().map(|slot| &slot.target).collect::<Vec<_>>(),
        schematic
            .iter()
            .map(|slot| &slot.target)
            .collect::<Vec<_>>(),
    );
    let marked = key_slots(Some(&keypad_marker_asset()), mx_keypad());
    // The draft's right paging marker sits ~6 px left of that key's centre,
    // so the cross-check stops at the left paging key.
    for (stock, marked) in stock.iter().zip(&marked).take(10) {
        assert!(
            (stock.point.x_frac - marked.point.x_frac).abs() < 0.005
                && (stock.point.y_frac - marked.point.y_frac).abs() < 0.005,
            "{} sits off its authored marker",
            stock.legend
        );
    }
}

/// A re-staged render is not the one the keys were measured on: rather than
/// land the keys off the physical ones, the keypad keeps its schematic.
#[test]
fn a_restaged_keypad_render_falls_back_to_the_keycap_map() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    let surface = key_surface(Some(&stock_keypad_asset((1057, 1227))), mx_keypad());

    assert!(matches!(surface, KeySurface::Keypad(_)));
}

/// The OpenLogi-authored c354 depot marks every key on the front render:
/// the nine grid keys by `_g<index>` (wire order, row-major) and the two
/// paging controls by their CIDs. Those caps go onto the render — positions
/// as authored, since keypad markers need no Logitech canvas correction —
/// in physical reading order.
#[test]
fn grid_markers_put_the_keypad_caps_on_the_photo() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    let slots = key_slots(Some(&keypad_marker_asset()), mx_keypad());

    assert_eq!(
        slots.len(),
        11,
        "the whole device is marked, so no key falls back to the schematic"
    );
    assert!(
        !slots
            .iter()
            .any(|slot| matches!(slot.target, KeyTarget::FunctionKey(_))),
        "a keypad binds its own keys, never the global F-row"
    );
    assert_eq!(slots[0].target, KeyTarget::grid(GridKey::TopLeft));
    assert_eq!(slots[0].legend, "Grid Key (Top Left)");
    assert_approx_eq(slots[0].point.x_frac, 24.92 / 100.);
    assert_approx_eq(slots[0].point.y_frac, 20.77 / 100.);
    assert_eq!(slots[4].target, KeyTarget::grid(GridKey::Middle));
    assert_eq!(slots[8].target, KeyTarget::grid(GridKey::BottomRight));
    assert_eq!(
        slots[9].target,
        KeyTarget::Control(ButtonId::control(0x01a1)),
        "the paging keys join as the controls they are, under the grid"
    );
    assert_eq!(slots[9].legend, "Left Paging Button");
    assert_eq!(
        slots[10].target,
        KeyTarget::Control(ButtonId::control(0x01a2))
    );
    assert!(
        slots[9].point.y_frac > slots[6].point.y_frac,
        "the paging row sits below the grid rows, in left-to-right order"
    );
}

/// The photo keys must keep the render's gaps: at the authored positions
/// and the render they were measured on, no two keys may overlap.
#[test]
fn photo_keys_keep_the_render_gaps_between_them() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let render = (935., 1105.);
    let slots = key_slots(Some(&keypad_marker_asset()), mx_keypad());

    for (a, b) in slots.iter().zip(&slots[1..]) {
        let (one, two) = (
            photo_key_shape(&a.target, render.0),
            photo_key_shape(&b.target, render.0),
        );
        let (w1, h1, w2, h2) = (one.w, one.h, two.w, two.h);
        let dx = (b.point.x_frac - a.point.x_frac).abs() * render.0;
        let dy = (b.point.y_frac - a.point.y_frac).abs() * render.1;
        assert!(
            2. * dx >= w1 + w2 || 2. * dy >= h1 + h2,
            "caps for {} and {} overlap",
            a.legend,
            b.legend
        );
    }
}

/// A Logi depot with control markers shows the keyboard's own controls —
/// one per marked control, named by the catalog, in physical reading order —
/// and never the OS-hook F-row. This is the MX Keys Mini: Easy-Switch takes
/// F1–F3, so the first marked key is F4's backlight control (#711).
#[test]
fn control_markers_become_control_slots_in_reading_order() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
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

    let slots = key_slots(Some(&asset), None);

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
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let asset = asset_with_controls(&[
        (0x0118, 93.6, 25.3),
        (0x0119, 93.6, 39.7),
        (0x01f3, 93.6, 54.0),
        (0x011c, 56.7, 11.0),
        (0x00d4, 63.0, 11.0),
    ]);

    let slots = key_slots(Some(&asset), None);

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

/// The same control marked twice at the same spot is one slot; Easy-Switch
/// host keys are not keys.
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

    let slots = key_slots(Some(&asset), None);

    assert_eq!(slots.len(), 1);
    assert_eq!(
        slots[0].target,
        KeyTarget::Control(ButtonId::control(0x010a))
    );
}

/// A control marked in both image groups is one slot wherever the copies
/// land after sorting, and the `device_keys_image` position wins even when
/// the `device_buttons_image` copy would sort first.
#[test]
fn control_slots_dedupe_across_image_groups_keeping_the_keys_image_position() {
    let mut asset = asset_with_controls(&[(0x010a, 53.5, 13.8), (0x0141, 30.0, 13.8)]);
    asset.metadata.images.push(ImageEntry {
        key: "device_buttons_image".to_string(),
        origin: Origin {
            width: 1872,
            height: 728,
        },
        // Earlier in reading order (further left) and on another row.
        assignments: vec![
            control_assignment(0x010a, 5.0, 13.8),
            control_assignment(0x010a, 53.5, 40.0),
        ],
    });

    let slots = key_slots(Some(&asset), None);

    let screen_capture: Vec<_> = slots
        .iter()
        .filter(|slot| slot.target == KeyTarget::Control(ButtonId::control(0x010a)))
        .collect();
    assert_eq!(slots.len(), 2);
    assert_eq!(screen_capture.len(), 1);
    assert_approx_eq(screen_capture[0].point.x_frac, 0.535 + 0.02);
    assert_approx_eq(screen_capture[0].point.y_frac, 0.138 + 0.023);
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
    let (w, h) = keyboard_render_size(Some(&mx), 900., KEYBOARD_W);
    assert_approx_eq(w, 700.);
    assert!((h - 700. * 728. / 1872.).abs() < 0.01);

    // G513 render (2760x1600) is far taller at the same width.
    let g513 = legacy_asset(&[], 0., (2760, 1600), (2760, 1600));
    let (w, h) = keyboard_render_size(Some(&g513), 900., KEYBOARD_W);
    assert_approx_eq(w, 700.);
    assert!((h - 700. * 1600. / 2760.).abs() < 0.01);

    // A short viewport shrinks the render instead of overflowing it.
    let (w, h) = keyboard_render_size(Some(&g513), 500., KEYBOARD_W);
    assert_approx_eq(h, KEYBOARD_MIN_IMG_H);
    assert!((w - KEYBOARD_MIN_IMG_H * 2760. / 1600.).abs() < 0.01);

    assert_eq!(
        keyboard_render_size(None, 900., KEYBOARD_W),
        FALLBACK_KEYBOARD_SIZE
    );
}

/// A card's box as `(left, top, right, bottom)`, grown by `margin`.
fn card_box(card: &CardRect, margin: f32) -> (f32, f32, f32, f32) {
    (
        card.left - margin,
        card.top - margin,
        card.left + card.width + margin,
        card.top + KEY_CALLOUT_H + margin,
    )
}

#[test]
fn side_cards_take_their_room_from_the_render_not_from_the_window() {
    let side = 152.;
    // Without cards beside the board nothing changes.
    assert_approx_eq(keyboard_max_width(800., 0.), KEYBOARD_W);
    // A roomy window keeps the full render beside the band.
    assert_approx_eq(keyboard_max_width(1400., side), KEYBOARD_W);
    // Where the render alone fitted, the render shrinks so the pane still fits.
    let w = keyboard_max_width(1000., side);
    assert_approx_eq(w + side, 1000. - KEYS_HORIZONTAL_RESERVE);
    // Where it did not, the pane stays as wide as the render alone was.
    assert_approx_eq(keyboard_max_width(800., side) + side, KEYBOARD_W);
}

/// Points sampled along each leader segment when checking it against cards.
const SAMPLES: u16 = 400;

/// Cards never overlap, a leader keeps clear of every other card, and no two
/// leaders cross.
fn assert_clean_callouts(layout: &CalloutLayout) {
    for (a, card) in layout.cards.iter().enumerate() {
        for other in &layout.cards[a + 1..] {
            let (l1, t1, r1, b1) = card_box(card, -0.01);
            let (l2, t2, r2, b2) = card_box(other, -0.01);
            assert!(
                !(l1 < r2 && l2 < r1 && t1 < b2 && t2 < b1),
                "cards overlap: {card:?} / {other:?}"
            );
        }
    }
    for (i, leader) in layout.leaders.iter().enumerate() {
        for (j, card) in layout.cards.iter().enumerate() {
            if i == j {
                continue;
            }
            let (left, top, right, bottom) = card_box(card, 6.);
            for segment in leader.windows(2) {
                let ((x0, y0), (x1, y1)) = (segment[0], segment[1]);
                for step in 0..=SAMPLES {
                    let t = f32::from(step) / f32::from(SAMPLES);
                    let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                    assert!(
                        !(x > left && x < right && y > top && y < bottom),
                        "leader {i} runs through card {j} at ({x}, {y})"
                    );
                }
            }
        }
        for other in &layout.leaders[i + 1..] {
            assert!(
                !routes_cross(leader, other),
                "leaders cross: {leader:?} / {other:?}"
            );
        }
    }
}

#[test]
fn the_fallback_f_row_fits_its_callouts_without_overlap_or_crossing() {
    let markers: Vec<(f32, f32)> = key_points(None)
        .into_iter()
        .map(|point| (point.x_frac, point.y_frac))
        .collect();
    let layout = callout_layout(&markers, KEYBOARD_W, 272.);

    assert_eq!(layout.pane_w, KEYBOARD_W);
    assert_clean_callouts(&layout);
    for card in &layout.cards {
        assert!(card.left >= -0.01 && card.left + card.width <= KEYBOARD_W + 0.01);
    }
}

/// The MX Mechanical Mini's depot marks twelve top-row keys and the four
/// navigation keys down the right edge.
#[test]
fn cards_sit_straight_above_or_beside_their_keys_when_there_is_room() {
    let top_xs = [
        25.9, 32., 38.15, 44.4, 50.5, 56.7, 63., 69., 75.2, 81.3, 87.35, 93.6,
    ];
    let mut markers: Vec<(f32, f32)> = top_xs.iter().map(|x| (x / 100. + 0.02, 0.133)).collect();
    markers.extend([25.3, 39.7, 54., 68.7].map(|y| (0.956, y / 100. + 0.023)));
    let (img_w, img_h) = (700., 270.);
    let layout = callout_layout(&markers, img_w, img_h);

    assert_clean_callouts(&layout);
    for (i, &(x, _)) in markers.iter().enumerate().take(top_xs.len()) {
        let card = layout.cards[i];
        assert!(
            (card.left + card.width / 2. - x * img_w).abs() < 0.5,
            "card {i} is not above its key"
        );
        assert_eq!(layout.leaders[i].len(), 2, "leader {i} is not straight");
        assert!(card.width > 60., "cards use the room two pitches give");
    }
    for i in top_xs.len()..markers.len() {
        assert!(
            layout.cards[i].left >= img_w,
            "card {i} is not beside the board"
        );
    }
    assert!(layout.pane_w > img_w);
    assert_eq!(layout.image_left, 0.);
}

#[test]
fn a_crowded_row_pushes_cards_apart_without_crossing() {
    // A G513-class render packs Esc–F12 into the left half of the image.
    let markers: Vec<(f32, f32)> = (0..13u8)
        .map(|i| (0.05 + f32::from(i) * 0.035, 0.12))
        .collect();
    let layout = callout_layout(&markers, KEYBOARD_W, 400.);

    assert_clean_callouts(&layout);
}

#[test]
fn a_key_down_the_left_edge_moves_the_board_right_for_its_card() {
    let markers = [(0.3, 0.1), (0.5, 0.1), (0.04, 0.5)];
    let layout = callout_layout(&markers, 600., 240.);

    assert!(layout.image_left > 0.);
    assert_eq!(layout.cards[2].left, 0.);
    assert!(layout.pane_w > 600. + layout.image_left - 0.01);
    assert_clean_callouts(&layout);
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
        side_view: None,
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
        side_view: None,
    }
}

/// The c354 depot as authored: `device_keys_image` marks the nine grid keys
/// row-major (`_g1`…`_g9`, at centres measured off the render) and the two
/// paging controls by CID.
fn keypad_marker_asset() -> ResolvedAsset {
    let mut assignments: Vec<Assignment> = [
        (1, 24.92, 20.77),
        (2, 50.33, 20.77),
        (3, 75.73, 20.77),
        (4, 24.92, 42.27),
        (5, 50.33, 42.27),
        (6, 75.73, 42.27),
        (7, 24.92, 63.84),
        (8, 50.33, 63.84),
        (9, 75.73, 63.84),
    ]
    .into_iter()
    .map(|(index, x, y)| Assignment {
        slot_id: format!("mx-creative-keypad-c354_g{index}"),
        slot_name: String::new(),
        marker: Point { x, y },
        label: Direction { x: 0, y: 0 },
    })
    .collect();
    assignments.push(control_assignment(0x01a1, 25.20, 82.05));
    assignments.push(control_assignment(0x01a2, 49.91, 82.04));
    ResolvedAsset {
        depot: "mx_creative_keypad".to_string(),
        display_name: "MX Creative Keypad".to_string(),
        kind: Some(DeviceKind::Numpad),
        image_path: PathBuf::from("/tmp/mx-creative-keypad.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata {
            images: vec![ImageEntry {
                key: "device_keys_image".to_string(),
                origin: Origin {
                    width: 935,
                    height: 1105,
                },
                assignments,
            }],
        },
        png_width: 935,
        png_height: 1105,
        side_view: None,
    }
}

/// The keypad depot as Logi ships it: the front render, no key markers.
fn stock_keypad_asset(png: (u32, u32)) -> ResolvedAsset {
    ResolvedAsset {
        metadata: Metadata { images: Vec::new() },
        png_width: png.0,
        png_height: png.1,
        ..keypad_marker_asset()
    }
}

fn assert_approx_eq(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.0001,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn an_open_application_binding_selects_its_power_user_row() {
    let open = Action::OpenApplication(
        openlogi_core::binding::ApplicationTarget::new("/Applications/Calculator.app", "")
            .expect("non-empty path"),
    );
    assert!(power_user_selected(
        Some(&open),
        PowerUserKind::OpenApplication
    ));
    assert!(!power_user_selected(Some(&open), PowerUserKind::TypeText));
    assert!(!power_user_selected(
        Some(&Action::Copy),
        PowerUserKind::OpenApplication
    ));
}

#[gpui::test]
fn open_application_editor_binds_the_selected_key(cx: &mut gpui::TestAppContext) {
    use crate::services::i18n::LOCALE_LOCK;
    use gpui::{Modifiers, VisualTestContext, size};
    use openlogi_core::binding::ApplicationTarget;

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).expect(selector);
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    fn f1_binding(cx: &mut VisualTestContext, trigger: &KeyTarget) -> Option<Action> {
        cx.update(|_, cx| bound_action(AppState::try_read(cx).unwrap(), trigger))
    }

    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        let resolver = crate::services::assets::AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            AppState::new(crate::state::Sources::in_memory(
                openlogi_core::config::Config::ephemeral(),
                &resolver,
                commands,
            ))
        });
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1200.), px(900.)));
    // Slot 1 of the fallback F-row is F1.
    let f1 = key_slots(None, None)[1].target.clone();
    view.update(cx, |view, cx| {
        view.select_key(Some(1), cx);
        view.open_editor(PowerUserKind::OpenApplication, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));

    // A blank path is rejected in place and binds nothing.
    click(cx, "key-editor-custom-application-add");
    view.read_with(cx, |view, _| {
        assert!(view.application_invalid);
        assert_eq!(view.active_editor, Some(PowerUserKind::OpenApplication));
    });
    assert!(
        cx.debug_bounds("key-editor-custom-application-error")
            .is_some()
    );
    assert_eq!(f1_binding(cx, &f1), None);

    click(cx, "key-editor-custom-application-input");
    cx.simulate_input("/Applications/Calculator.app");
    click(cx, "key-editor-custom-application-add");
    let expected = Action::OpenApplication(
        ApplicationTarget::new("/Applications/Calculator.app", "").expect("non-empty path"),
    );
    assert_eq!(f1_binding(cx, &f1), Some(expected));
    view.read_with(cx, |view, _| {
        assert_eq!(view.active_editor, None);
        assert!(!view.application_invalid);
    });

    // Reopening the editor seeds the bound path.
    view.update(cx, |view, cx| {
        view.open_editor(PowerUserKind::OpenApplication, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.text_state().expect("editor input").read(cx).value(),
            "/Applications/Calculator.app"
        );
    });

    // Re-adding the unchanged path keeps a saved display name.
    let named = Action::OpenApplication(
        ApplicationTarget::new("/Applications/Calculator.app", "My calculator").expect("valid"),
    );
    cx.update(|_, cx| {
        AppState::apply(cx, |state| {
            commit_key_action(state, &f1, Some(named.clone()))
        });
    });
    click(cx, "key-editor-custom-application-add");
    assert_eq!(f1_binding(cx, &f1), Some(named));

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// The view edits grid keys on its chosen page by moving their targets there;
/// the paging buttons and F-row keys have no pages and stay as they are.
#[test]
fn only_grid_targets_move_to_the_edited_page() {
    use openlogi_core::binding::{GridSlot, KeypadPage};
    let second = KeypadPage::new(2).unwrap();

    assert_eq!(
        KeyTarget::grid(GridKey::Middle).on_page(second),
        KeyTarget::Grid(GridSlot {
            page: second,
            key: GridKey::Middle,
        })
    );
    let paging = KeyTarget::Control(ButtonId::Control(Cid::RIGHT_PAGING));
    assert_eq!(paging.clone().on_page(second), paging);
    // Every keypad layout slot starts on page 1, whatever page is edited.
    let keypad = key_surface(None, mx_keypad()).slots();
    assert!(
        keypad
            .iter()
            .filter_map(|slot| match &slot.target {
                KeyTarget::Grid(grid) => Some(grid.page),
                _ => None,
            })
            .all(|page| page == KeypadPage::FIRST)
    );
}

/// A key-row view whose workflow editor has a draft open on keypad page 2.
fn view_with_a_draft_on_page_two(cx: &mut gpui::TestAppContext) -> Entity<FunctionRowView> {
    use crate::services::assets::AssetResolver;
    use crate::state::Sources;

    cx.update(|cx| {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources::in_memory(
                openlogi_core::config::Config::ephemeral(),
                &resolver,
                commands,
            ))
        });
        AppState::set_global(state, cx);
        cx.new(|cx| {
            let mut view = FunctionRowView::new(cx);
            view.set_keypad_page(KeypadPage::new(2).unwrap(), cx);
            view.select_key(Some(0), cx);
            view.open_editor(PowerUserKind::Workflow, cx);
            view.push_workflow_step(WorkflowStep::Delay { millis: 50 }, cx);
            view
        })
    })
}

#[gpui::test]
fn turning_the_page_drops_a_draft_for_the_old_page(cx: &mut gpui::TestAppContext) {
    let view = view_with_a_draft_on_page_two(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.set_keypad_page(KeypadPage::FIRST, cx);
            assert_eq!(view.active_editor, None);
            assert!(view.workflow_draft().is_empty());
        });
    });
}

#[gpui::test]
fn removing_the_edited_page_drops_its_draft(cx: &mut gpui::TestAppContext) {
    let view = view_with_a_draft_on_page_two(cx);
    cx.update(|cx| {
        view.update(cx, |view, _| {
            // No device has a second page, so the edited page clamps to 1.
            view.sync_edit_scope(None);
            assert_eq!(view.keypad_page, KeypadPage::FIRST);
            assert_eq!(view.active_editor, None);
            assert!(view.workflow_draft().is_empty());
        });
    });
}

#[gpui::test]
fn switching_the_app_profile_drops_a_draft_for_the_old_one(cx: &mut gpui::TestAppContext) {
    let (state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    cx.update(|cx| {
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
        let view = cx.new(FunctionRowView::new);
        view.update(cx, |view, cx| {
            view.sync_edit_scope(AppState::try_read(cx));
            view.select_key(Some(0), cx);
            view.open_editor(PowerUserKind::Workflow, cx);
            view.push_workflow_step(WorkflowStep::Delay { millis: 50 }, cx);
        });

        AppState::global(cx).update(cx, |state, _| {
            let _ = state.set_editing_app(Some("com.apple.Safari".into()));
        });
        view.update(cx, |view, cx| {
            view.sync_edit_scope(AppState::try_read(cx));
            assert_eq!(view.active_editor, None);
            assert!(view.workflow_draft().is_empty());
            // The key stays selected: only the draft belonged to the profile.
            assert_eq!(view.selected_key, Some(0));
        });
    });
}
